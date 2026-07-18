//! Conditional highlighting (F42): view-only, theme-aware decoration of cells
//! and rows driven by a prioritized set of rules. Highlighting NEVER touches
//! data — evaluating a rule reads the document (and cached analysis results),
//! it never writes a cell, never enters the undo stack, and never makes the
//! document dirty.
//!
//! ## Model
//!
//! A [`HighlightRule`] pairs a [`HighlightCondition`] (the predicate) with a
//! [`HighlightTarget`] (what gets decorated when it matches), a numeric
//! `priority` (higher wins per overlapping target; ties break by rule id), and
//! a theme-aware [`Decoration`] carrying a SEMANTIC tone (accent/warn/error/…)
//! rather than a raw colour, so light and dark themes both stay readable.
//!
//! ## Evaluation & caching
//!
//! Every enabled rule is evaluated in Rust against the document plus, for the
//! analysis-backed conditions, the relevant cached analysis result. Each rule's
//! match set is cached and invalidated by the triple
//! `(rule revision, data revision, analysis revision)`:
//!
//! * **rule revision** — a per-rule counter bumped only when THAT rule's
//!   definition changes, so editing one rule never invalidates another's cache;
//! * **data revision** — the max of the rule's scanned columns' revisions (or
//!   the whole-document revision for row-level / all-column rules), so an edit
//!   to an unrelated column keeps a column-scoped rule's cache warm;
//! * **analysis revision** — a fingerprint of the analysis cache the condition
//!   reads (outlier/cross-column/diagnostics), so re-running an analysis
//!   refreshes the highlight without a data change.
//!
//! The changed-since-save condition is *volatile* — it reads the dirty-cell set,
//! which a save clears without moving any tracked revision — so it is recomputed
//! on every query rather than cached. The F40 annotation conditions (bookmarked
//! / flagged / tagged) are cached like the rest: their `analysis revision` is the
//! annotation store's revision, so an annotation edit invalidates them while a
//! plain scroll reuses the warm match sets.
//!
//! ## Windowed transfer
//!
//! The front end never receives a per-cell stream. It asks for one bounded
//! window (the visible rows) through [`DocState::window`]; the backend resolves
//! every overlapping rule server-side, flattens them by priority into one
//! winning decoration per cell, and returns only the decorated cells. Scrolling
//! a million-row document reuses the cached match sets — no recomputation, no
//! per-cell IPC.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};

use crate::analyze;
use crate::crossval::CachedCrossVal;
use crate::diagnostics::DiagnosticsReport;
use crate::document::Document;
use crate::dto::ExportScope;
use crate::error::{AppError, AppResult};
use crate::job::JobCtx;
use crate::outlier::CachedOutlier;
use crate::schema::{self, CellState, ColumnSchema, LogicalType};

/// A window larger than this is refused: the flattener allocates a dense
/// `rows × columns` grid, so an unbounded window would blow up memory. The
/// front end always fetches the visible viewport, far under this cap.
const MAX_WINDOW_ROWS: usize = 20_000;

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Rule model (wire DTOs, camelCase)
// ---------------------------------------------------------------------------

/// Visual weight of a decoration, mapped to theme tokens on the front end.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Emphasis {
    /// A faint tint (e.g. a soft background wash).
    Subtle,
    #[default]
    Normal,
    /// The most prominent treatment (e.g. a solid badge / strong fill).
    Strong,
}

/// Semantic colour role. Deliberately NOT a raw colour: the theme + gridTheme
/// map each tone to a readable pair for light and dark, so no rule stores an
/// arbitrary hex value by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SemanticTone {
    #[default]
    Accent,
    Info,
    Warn,
    Error,
    Success,
    Neutral,
}

/// Optional text-weight override for a decorated cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextStyle {
    #[default]
    Normal,
    Bold,
    Italic,
}

/// How a matched cell is decorated. Theme-aware and colour-free by default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Decoration {
    #[serde(default)]
    pub tone: SemanticTone,
    #[serde(default)]
    pub emphasis: Emphasis,
    /// Optional short glyph/name the front end renders as a leading icon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default)]
    pub text_style: TextStyle,
}

impl Default for Decoration {
    fn default() -> Self {
        Decoration {
            tone: SemanticTone::Accent,
            emphasis: Emphasis::Normal,
            icon: None,
            text_style: TextStyle::Normal,
        }
    }
}

/// What a matching rule decorates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HighlightTarget {
    /// Only the cell that matched (row-level conditions decorate the whole row,
    /// since they have no single cell).
    Cell,
    /// The whole row of any matching cell.
    Row,
    /// Matching cells, restricted to these columns (by stable column id).
    Columns { column_ids: Vec<String> },
}

/// The condition a rule tests. Column-scoped predicates carry an optional
/// `column_id`; when omitted they apply to the columns named by a `columns`
/// target, or to every column for a `cell`/`row` target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum HighlightCondition {
    /// Cell value equals `value` (compared trimmed).
    Equals {
        #[serde(default)]
        column_id: Option<String>,
        value: String,
        #[serde(default)]
        case_sensitive: bool,
    },
    /// Cell value does NOT equal `value` (compared trimmed).
    NotEquals {
        #[serde(default)]
        column_id: Option<String>,
        value: String,
        #[serde(default)]
        case_sensitive: bool,
    },
    /// Cell value contains `value` as a substring.
    Contains {
        #[serde(default)]
        column_id: Option<String>,
        value: String,
        #[serde(default)]
        case_sensitive: bool,
    },
    /// Cell value matches a regular expression (unanchored; use `^…$` to
    /// anchor). Compile errors are surfaced by rule validation.
    Regex {
        #[serde(default)]
        column_id: Option<String>,
        pattern: String,
        #[serde(default)]
        case_sensitive: bool,
    },
    /// Numeric value within `[min, max]` (either bound optional). Uses the
    /// declared logical type when the column has one, else the numeric
    /// heuristic; non-numeric and blank cells never match.
    NumericRange {
        #[serde(default)]
        column_id: Option<String>,
        #[serde(default)]
        min: Option<f64>,
        #[serde(default)]
        max: Option<f64>,
        #[serde(default = "default_true")]
        inclusive: bool,
    },
    /// Date/datetime within `[min, max]` (ISO-ish strings, either optional).
    DateRange {
        #[serde(default)]
        column_id: Option<String>,
        #[serde(default)]
        min: Option<String>,
        #[serde(default)]
        max: Option<String>,
    },
    /// Blank / null: empty, whitespace-only, a missing field, or a configured
    /// null token (F31 classify semantics).
    Blank {
        #[serde(default)]
        column_id: Option<String>,
    },
    /// Present but invalid for the column's DECLARED logical type (F31). A
    /// column with no declared schema never matches.
    Invalid {
        #[serde(default)]
        column_id: Option<String>,
    },
    /// A value repeated within its column, using the shared dedup normalization
    /// (blank values are never treated as duplicates).
    Duplicate {
        #[serde(default)]
        column_id: Option<String>,
        #[serde(default)]
        trim: bool,
        #[serde(default)]
        case_insensitive: bool,
        #[serde(default)]
        collapse_whitespace: bool,
    },
    /// Cells/rows flagged by the document's last diagnostics scan (F02). With
    /// `issue_id`, one specific issue; otherwise every row-filterable issue.
    /// Empty until a scan has run.
    Diagnostic {
        #[serde(default)]
        issue_id: Option<String>,
    },
    /// Rows failing the document's last cross-column validation (F27). With
    /// `rule_index`, one specific rule; otherwise any rule. Empty until a scan
    /// has run.
    CrossColumn {
        #[serde(default)]
        rule_index: Option<usize>,
    },
    /// Cells flagged by the document's last outlier scan (F30). Empty until a
    /// scan has run.
    Outlier,
    /// Cells changed since the last save (best-effort; a save clears these).
    ChangedSinceSave {
        #[serde(default)]
        column_id: Option<String>,
    },
    // ---- F40 row annotations (wired beneath this stage) ----------------
    // Backed by the annotation store's resolved marks (see `AnnotationMatches`).
    // Only MATCHED rows contribute — an ambiguous / orphaned annotation never
    // decorates a row. Empty when the document carries no annotations.
    /// Rows the user has bookmarked (starred) in F40 annotations.
    Bookmarked,
    /// Rows the user has flagged in F40 annotations. F40 flags carry no label,
    /// so this decorates every flagged row.
    Flagged,
    /// Rows carrying the named F40 tag.
    Tagged { tag: String },
}

/// One conditional-highlighting rule. Persisted verbatim inside named views
/// (F12) and file profiles (F08); carries no cell data, so it round-trips
/// through the project store's no-cell-data guard unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HighlightRule {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub condition: HighlightCondition,
    pub target: HighlightTarget,
    /// Higher wins per overlapping target; ties break by `id`.
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub decoration: Decoration,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl HighlightRule {
    /// The condition's own column scope, when it carries one.
    fn condition_column_id(&self) -> Option<&str> {
        match &self.condition {
            HighlightCondition::Equals { column_id, .. }
            | HighlightCondition::NotEquals { column_id, .. }
            | HighlightCondition::Contains { column_id, .. }
            | HighlightCondition::Regex { column_id, .. }
            | HighlightCondition::NumericRange { column_id, .. }
            | HighlightCondition::DateRange { column_id, .. }
            | HighlightCondition::Blank { column_id }
            | HighlightCondition::Invalid { column_id }
            | HighlightCondition::Duplicate { column_id, .. }
            | HighlightCondition::ChangedSinceSave { column_id } => column_id.as_deref(),
            _ => None,
        }
    }

    /// Whether this rule is recomputed on every query instead of cached: its
    /// inputs can change without moving any tracked revision (the dirty-cell set
    /// a save clears). The annotation conditions are NOT volatile — they cache
    /// against the annotation store's revision like the analysis-backed ones.
    fn is_volatile(&self) -> bool {
        matches!(self.condition, HighlightCondition::ChangedSinceSave { .. })
    }

    /// Whether the condition reads the document per-cell (as opposed to a
    /// row-level or analysis-backed condition), so its data revision can be
    /// scoped to just the columns it scans.
    fn is_cell_predicate(&self) -> bool {
        matches!(
            self.condition,
            HighlightCondition::Equals { .. }
                | HighlightCondition::NotEquals { .. }
                | HighlightCondition::Contains { .. }
                | HighlightCondition::Regex { .. }
                | HighlightCondition::NumericRange { .. }
                | HighlightCondition::DateRange { .. }
                | HighlightCondition::Blank { .. }
                | HighlightCondition::Invalid { .. }
                | HighlightCondition::Duplicate { .. }
                | HighlightCondition::ChangedSinceSave { .. }
        )
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate a rule coming from the front end before it is stored. Regex
/// compile errors and unusable ranges are surfaced; unknown column ids are
/// tolerated (they simply match nothing, so a saved rule survives a column
/// rename until the user re-points it).
pub fn validate_rule(rule: &HighlightRule) -> AppResult<()> {
    if rule.id.trim().is_empty() {
        return Err(AppError::invalid("highlight rule id must not be empty"));
    }
    match &rule.condition {
        HighlightCondition::Regex { pattern, .. } => {
            build_regex(pattern, /*case_sensitive*/ false).map(|_| ())?;
        }
        HighlightCondition::NumericRange { min, max, .. } => {
            if let (Some(lo), Some(hi)) = (min, max) {
                if !lo.is_finite() || !hi.is_finite() {
                    return Err(AppError::invalid("numeric range bounds must be finite"));
                }
                if lo > hi {
                    return Err(AppError::invalid("numeric range min must be ≤ max"));
                }
            }
        }
        HighlightCondition::DateRange { min, max, .. } => {
            let lo = parse_bound_date(min.as_deref())?;
            let hi = parse_bound_date(max.as_deref())?;
            if let (Some(lo), Some(hi)) = (lo, hi) {
                if lo > hi {
                    return Err(AppError::invalid("date range min must be ≤ max"));
                }
            }
        }
        HighlightCondition::Tagged { tag } if tag.trim().is_empty() => {
            return Err(AppError::invalid("tag must not be empty"));
        }
        _ => {}
    }
    if let HighlightTarget::Columns { column_ids } = &rule.target {
        if column_ids.is_empty() {
            return Err(AppError::invalid(
                "a columns target needs at least one column id",
            ));
        }
    }
    Ok(())
}

/// Compile a regex for a condition, honouring case sensitivity.
fn build_regex(pattern: &str, case_sensitive: bool) -> AppResult<regex::Regex> {
    regex::RegexBuilder::new(pattern)
        .case_insensitive(!case_sensitive)
        .build()
        .map_err(|e| AppError::invalid(format!("invalid regular expression: {e}")))
}

/// Parse an optional ISO-ish date/datetime bound, erroring on a non-empty but
/// unparseable value.
fn parse_bound_date(raw: Option<&str>) -> AppResult<Option<NaiveDateTime>> {
    match raw.map(str::trim) {
        None | Some("") => Ok(None),
        Some(s) => analyze::parse_date(s)
            .map(Some)
            .ok_or_else(|| AppError::invalid(format!("\"{s}\" is not a recognised date"))),
    }
}

// ---------------------------------------------------------------------------
// Analysis context (cached analysis results the conditions consume)
// ---------------------------------------------------------------------------

/// A snapshot of the analysis caches a document currently has, cloned by the
/// command layer and passed into evaluation. Keeping it a plain value (rather
/// than the Tauri `State` handles) makes the whole evaluator unit-testable.
#[derive(Debug, Clone, Default)]
pub struct AnalysisContext {
    pub outlier: Option<CachedOutlier>,
    pub crossval: Option<CachedCrossVal>,
    pub diagnostics: Option<DiagnosticsReport>,
    /// Resolved F40 annotation marks (absolute rows), when the document has any
    /// annotation-backed rule. `None` when no annotation condition is active, so
    /// the rematch that builds it is skipped entirely on the common path.
    pub annotations: Option<AnnotationMatches>,
}

/// The absolute rows an F40 annotation condition decorates, resolved once from
/// the annotation store and shared across a query's rules. The `revision` is the
/// annotation store's revision — the analysis-revision component of the three
/// annotation conditions' cache key, so an annotation edit invalidates them.
#[derive(Debug, Clone, Default)]
pub struct AnnotationMatches {
    /// The annotation-store revision these sets were resolved at.
    pub revision: u64,
    /// Sorted absolute rows that are bookmarked (starred).
    pub starred: Vec<usize>,
    /// Sorted absolute rows that are flagged.
    pub flagged: Vec<usize>,
    /// Tag name → sorted absolute rows carrying it.
    pub tagged: HashMap<String, Vec<usize>>,
}

/// Stable-ish fingerprint of a serializable analysis input, used to detect an
/// analysis-cache change that leaves the document revision untouched (a rescan
/// with a different spec at the same data revision).
fn fingerprint<T: Serialize>(value: &T) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(value)
        .unwrap_or_default()
        .hash(&mut hasher);
    hasher.finish()
}

/// The analysis revision component of a condition's cache key.
fn analysis_revision(condition: &HighlightCondition, actx: &AnalysisContext) -> u64 {
    match condition {
        HighlightCondition::Outlier => actx
            .outlier
            .as_ref()
            .map(|(spec, report)| fingerprint(spec) ^ report.revision)
            .unwrap_or(0),
        HighlightCondition::CrossColumn { .. } => actx
            .crossval
            .as_ref()
            .map(|(rules, report)| fingerprint(rules) ^ report.revision)
            .unwrap_or(0),
        HighlightCondition::Diagnostic { .. } => actx
            .diagnostics
            .as_ref()
            // +1 so "present at revision 0" is distinguishable from "absent".
            .map(|report| report.revision.wrapping_add(1))
            .unwrap_or(0),
        HighlightCondition::Bookmarked
        | HighlightCondition::Flagged
        | HighlightCondition::Tagged { .. } => actx
            .annotations
            .as_ref()
            // +1 so "present at revision 0" is distinguishable from "absent".
            .map(|a| a.revision.wrapping_add(1))
            .unwrap_or(0),
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Match sets
// ---------------------------------------------------------------------------

/// The raw geometry a condition produces, before the target projects it.
enum RawMatches {
    /// Cells with a definite column (absolute row, column index).
    Cells(Vec<(usize, usize)>),
    /// Whole rows (a row-level condition with no single column).
    Rows(Vec<usize>),
}

/// A rule's projected, cached match geometry (absolute coordinates).
#[derive(Debug, Clone, PartialEq)]
enum MatchSet {
    /// Entire rows are decorated; sorted, de-duplicated absolute row indices.
    Rows(Vec<usize>),
    /// Specific cells: absolute row → sorted, de-duplicated column indices.
    Cells(HashMap<usize, Vec<usize>>),
}

/// How a rule covers one absolute row, for the window flattener.
enum RowCoverage<'a> {
    None,
    /// Every column of the row (a row-target / row-level match).
    All,
    /// Just these column indices.
    Cols(&'a [usize]),
}

impl MatchSet {
    fn coverage(&self, row: usize) -> RowCoverage<'_> {
        match self {
            MatchSet::Rows(rows) => {
                if rows.binary_search(&row).is_ok() {
                    RowCoverage::All
                } else {
                    RowCoverage::None
                }
            }
            MatchSet::Cells(map) => match map.get(&row) {
                Some(cols) => RowCoverage::Cols(cols),
                None => RowCoverage::None,
            },
        }
    }

    /// Whether this rule decorates exactly cell `(row, col)`.
    fn matches_cell(&self, row: usize, col: usize) -> bool {
        match self {
            MatchSet::Rows(rows) => rows.binary_search(&row).is_ok(),
            MatchSet::Cells(map) => map
                .get(&row)
                .is_some_and(|cols| cols.binary_search(&col).is_ok()),
        }
    }

    /// The number of decorated TARGETS: rows for a whole-row match, cells for a
    /// cell / columns match — matching the unit the rule paints, for the
    /// dialog's live per-rule count.
    fn count(&self) -> usize {
        match self {
            MatchSet::Rows(rows) => rows.len(),
            MatchSet::Cells(map) => map.values().map(|c| c.len()).sum(),
        }
    }
}

/// Project raw geometry through a target into a cached match set.
fn project(raw: RawMatches, target: &HighlightTarget, doc: &Document) -> MatchSet {
    match target {
        HighlightTarget::Row => {
            let mut rows: Vec<usize> = match raw {
                RawMatches::Rows(rows) => rows,
                RawMatches::Cells(cells) => cells.into_iter().map(|(r, _)| r).collect(),
            };
            rows.sort_unstable();
            rows.dedup();
            MatchSet::Rows(rows)
        }
        HighlightTarget::Cell => match raw {
            // A row-level condition has no single cell → decorate the whole row.
            RawMatches::Rows(mut rows) => {
                rows.sort_unstable();
                rows.dedup();
                MatchSet::Rows(rows)
            }
            RawMatches::Cells(cells) => MatchSet::Cells(group_cells(cells)),
        },
        HighlightTarget::Columns { column_ids } => {
            let cols = resolve_column_ids(doc, column_ids);
            let cells = match raw {
                RawMatches::Cells(cells) => cells
                    .into_iter()
                    .filter(|(_, c)| cols.contains(c))
                    .collect(),
                // Expand each matched row across the targeted columns.
                RawMatches::Rows(rows) => {
                    let mut out = Vec::with_capacity(rows.len() * cols.len());
                    for r in rows {
                        for &c in &cols {
                            out.push((r, c));
                        }
                    }
                    out
                }
            };
            MatchSet::Cells(group_cells(cells))
        }
    }
}

/// Group `(row, col)` pairs into a row → sorted-columns map.
fn group_cells(cells: Vec<(usize, usize)>) -> HashMap<usize, Vec<usize>> {
    let mut map: HashMap<usize, Vec<usize>> = HashMap::new();
    for (r, c) in cells {
        map.entry(r).or_default().push(c);
    }
    for cols in map.values_mut() {
        cols.sort_unstable();
        cols.dedup();
    }
    map
}

/// Resolve stable column ids to current indices (unknown ids dropped).
fn resolve_column_ids(doc: &Document, ids: &[String]) -> Vec<usize> {
    let column_ids = doc.column_ids();
    ids.iter()
        .filter_map(|id| column_ids.iter().position(|c| c == id))
        .collect()
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

/// The set of columns a cell-predicate condition scans: its own `column_id`
/// when set, else the target's columns, else every column.
fn predicate_columns(rule: &HighlightRule, doc: &Document) -> Vec<usize> {
    if let Some(id) = rule.condition_column_id() {
        return resolve_column_ids(doc, &[id.to_string()]);
    }
    match &rule.target {
        HighlightTarget::Columns { column_ids } => resolve_column_ids(doc, column_ids),
        _ => (0..doc.n_cols()).collect(),
    }
}

/// A prepared per-cell predicate, compiled once per scan.
enum CellTest {
    Equals {
        needle: String,
        case_sensitive: bool,
        negate: bool,
    },
    Contains {
        needle: String,
        case_sensitive: bool,
    },
    Regex(regex::Regex),
    NumericRange {
        min: Option<f64>,
        max: Option<f64>,
        inclusive: bool,
    },
    DateRange {
        min: Option<NaiveDateTime>,
        max: Option<NaiveDateTime>,
    },
    Blank,
    Invalid,
}

impl CellTest {
    fn test(&self, raw: &str, schema: Option<&ColumnSchema>) -> bool {
        match self {
            CellTest::Equals {
                needle,
                case_sensitive,
                negate,
            } => {
                let hit = text_eq(raw.trim(), needle.trim(), *case_sensitive);
                hit != *negate
            }
            CellTest::Contains {
                needle,
                case_sensitive,
            } => text_contains(raw, needle, *case_sensitive),
            CellTest::Regex(re) => re.is_match(raw),
            CellTest::NumericRange {
                min,
                max,
                inclusive,
            } => match read_number(raw, schema) {
                Some(n) => in_numeric_range(n, *min, *max, *inclusive),
                None => false,
            },
            CellTest::DateRange { min, max } => match read_date(raw, schema) {
                Some(d) => min.is_none_or(|lo| d >= lo) && max.is_none_or(|hi| d <= hi),
                None => false,
            },
            CellTest::Blank => matches!(
                classify_state(raw, schema),
                CellState::Empty | CellState::NullToken | CellState::Missing
            ),
            CellTest::Invalid => matches!(classify_state(raw, schema), CellState::Invalid(_)),
        }
    }
}

fn text_eq(a: &str, b: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        a == b
    } else {
        a.eq_ignore_ascii_case(b) || a.to_lowercase() == b.to_lowercase()
    }
}

fn text_contains(haystack: &str, needle: &str, case_sensitive: bool) -> bool {
    if case_sensitive {
        haystack.contains(needle)
    } else {
        haystack.to_lowercase().contains(&needle.to_lowercase())
    }
}

fn in_numeric_range(n: f64, min: Option<f64>, max: Option<f64>, inclusive: bool) -> bool {
    let lower = min.is_none_or(|lo| if inclusive { n >= lo } else { n > lo });
    let upper = max.is_none_or(|hi| if inclusive { n <= hi } else { n < hi });
    lower && upper
}

/// Read a cell as a number, preferring the declared logical type (locale
/// separators, null tokens) and falling back to the numeric heuristic.
fn read_number(raw: &str, schema: Option<&ColumnSchema>) -> Option<f64> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(s) = schema {
        if schema::is_null_token(s, t) {
            return None;
        }
        if s.logical_type.is_numeric() {
            return match schema::numeric_cell(s, t) {
                schema::NumericCell::Value(n) => Some(n),
                _ => None,
            };
        }
    }
    analyze::as_number(t)
}

/// Read a cell as a date/datetime, preferring the declared logical type.
fn read_date(raw: &str, schema: Option<&ColumnSchema>) -> Option<NaiveDateTime> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(s) = schema {
        if schema::is_null_token(s, t) {
            return None;
        }
        if s.logical_type.is_temporal() {
            return schema::temporal_cell(s, t);
        }
    }
    analyze::parse_date(t)
}

/// Classify a raw cell against its column schema (a default text schema when
/// none is declared, so blank still resolves and invalid never fires).
fn classify_state(raw: &str, schema: Option<&ColumnSchema>) -> CellState {
    match schema {
        Some(s) => schema::classify(Some(raw), s),
        None => {
            let default = ColumnSchema::new("", "", LogicalType::Text);
            schema::classify(Some(raw), &default)
        }
    }
}

/// Build the compiled predicate for a cell-predicate condition, or `None` when
/// the condition is not a per-cell predicate.
fn build_cell_test(condition: &HighlightCondition) -> AppResult<Option<CellTest>> {
    let test = match condition {
        HighlightCondition::Equals {
            value,
            case_sensitive,
            ..
        } => CellTest::Equals {
            needle: value.clone(),
            case_sensitive: *case_sensitive,
            negate: false,
        },
        HighlightCondition::NotEquals {
            value,
            case_sensitive,
            ..
        } => CellTest::Equals {
            needle: value.clone(),
            case_sensitive: *case_sensitive,
            negate: true,
        },
        HighlightCondition::Contains {
            value,
            case_sensitive,
            ..
        } => CellTest::Contains {
            needle: value.clone(),
            case_sensitive: *case_sensitive,
        },
        HighlightCondition::Regex {
            pattern,
            case_sensitive,
            ..
        } => CellTest::Regex(build_regex(pattern, *case_sensitive)?),
        HighlightCondition::NumericRange {
            min,
            max,
            inclusive,
            ..
        } => CellTest::NumericRange {
            min: *min,
            max: *max,
            inclusive: *inclusive,
        },
        HighlightCondition::DateRange { min, max, .. } => CellTest::DateRange {
            min: parse_bound_date(min.as_deref())?,
            max: parse_bound_date(max.as_deref())?,
        },
        HighlightCondition::Blank { .. } => CellTest::Blank,
        HighlightCondition::Invalid { .. } => CellTest::Invalid,
        _ => return Ok(None),
    };
    Ok(Some(test))
}

/// Scan `columns` over the whole document, collecting cells the predicate
/// accepts. Streams through the row-visit API (both backings).
fn scan_predicate(
    doc: &Document,
    columns: &[usize],
    test: &CellTest,
) -> AppResult<Vec<(usize, usize)>> {
    let mut cells = Vec::new();
    if columns.is_empty() {
        return Ok(cells);
    }
    let schemas: Vec<Option<ColumnSchema>> = columns
        .iter()
        .map(|&c| doc.column_schema_at(c).cloned())
        .collect();
    doc.visit_rows(0..doc.n_rows(), &mut |r, row| {
        for (&c, schema) in columns.iter().zip(schemas.iter()) {
            let cell = row.get(c).map(String::as_str).unwrap_or("");
            if test.test(cell, schema.as_ref()) {
                cells.push((r, c));
            }
        }
        Ok(true)
    })?;
    Ok(cells)
}

/// Duplicate-value cells for one column, reusing the shared dedup machinery
/// with the given normalization. Blank keys are excluded, so a blank cell is
/// never treated as a duplicate.
fn duplicate_cells(
    doc: &Document,
    col: usize,
    trim: bool,
    case_insensitive: bool,
    collapse_whitespace: bool,
) -> AppResult<Vec<(usize, usize)>> {
    let spec = crate::dedup::DedupSpec {
        key_columns: vec![col],
        trim,
        case_insensitive,
        collapse_whitespace,
        blank_keys_equal: false,
        exclude_blank_keys: true,
    };
    let rows = crate::dedup::duplicate_row_indices(doc, &spec, &ExportScope::All)?;
    Ok(rows.into_iter().map(|r| (r, col)).collect())
}

/// Evaluate one rule into its projected match set (absolute coordinates).
/// Analysis-backed conditions read the supplied [`AnalysisContext`] and, when
/// the underlying analysis has gone stale against the current document, resolve
/// to no matches rather than failing — highlighting is best-effort decoration
/// and must never break the grid.
fn evaluate_rule(
    rule: &HighlightRule,
    doc: &Document,
    actx: &AnalysisContext,
) -> AppResult<MatchSet> {
    let raw = match &rule.condition {
        // ----- per-cell predicates -----
        HighlightCondition::Equals { .. }
        | HighlightCondition::NotEquals { .. }
        | HighlightCondition::Contains { .. }
        | HighlightCondition::Regex { .. }
        | HighlightCondition::NumericRange { .. }
        | HighlightCondition::DateRange { .. }
        | HighlightCondition::Blank { .. }
        | HighlightCondition::Invalid { .. } => {
            let cols = predicate_columns(rule, doc);
            let test = build_cell_test(&rule.condition)?.expect("cell predicate");
            RawMatches::Cells(scan_predicate(doc, &cols, &test)?)
        }
        // ----- per-column duplicate values -----
        HighlightCondition::Duplicate {
            trim,
            case_insensitive,
            collapse_whitespace,
            ..
        } => {
            let cols = predicate_columns(rule, doc);
            let mut cells = Vec::new();
            for col in cols {
                cells.extend(duplicate_cells(
                    doc,
                    col,
                    *trim,
                    *case_insensitive,
                    *collapse_whitespace,
                )?);
            }
            RawMatches::Cells(cells)
        }
        // ----- changed since save (volatile; dirty-cell set) -----
        HighlightCondition::ChangedSinceSave { .. } => {
            let cols = predicate_columns(rule, doc);
            let allowed: Option<std::collections::HashSet<usize>> = match rule.condition_column_id()
            {
                Some(_) => Some(cols.iter().copied().collect()),
                None => match &rule.target {
                    HighlightTarget::Columns { .. } => Some(cols.iter().copied().collect()),
                    _ => None,
                },
            };
            let cells: Vec<(usize, usize)> = doc
                .dirty_cells()
                .iter()
                .filter(|(_, c)| allowed.as_ref().is_none_or(|set| set.contains(c)))
                .copied()
                .collect();
            RawMatches::Cells(cells)
        }
        // ----- outlier scan (F30) -----
        HighlightCondition::Outlier => match &actx.outlier {
            Some((spec, _report)) => {
                let cells = crate::outlier::flagged_rows(doc, spec)
                    .map(|rows| rows.into_iter().map(|r| (r, spec.column)).collect())
                    .unwrap_or_default();
                RawMatches::Cells(cells)
            }
            None => RawMatches::Rows(Vec::new()),
        },
        // ----- cross-column validation (F27) -----
        HighlightCondition::CrossColumn { rule_index } => match &actx.crossval {
            Some((rules, _report)) => {
                let rows =
                    crate::crossval::violating_rows(doc, rules, *rule_index).unwrap_or_default();
                RawMatches::Rows(rows)
            }
            None => RawMatches::Rows(Vec::new()),
        },
        // ----- diagnostics (F02) -----
        HighlightCondition::Diagnostic { issue_id } => match &actx.diagnostics {
            Some(report) => RawMatches::Rows(diagnostic_rows(doc, report, issue_id.as_deref())),
            None => RawMatches::Rows(Vec::new()),
        },
        // ----- F40 annotations (resolved marks from the annotation store) -----
        HighlightCondition::Bookmarked => RawMatches::Rows(
            actx.annotations
                .as_ref()
                .map(|a| a.starred.clone())
                .unwrap_or_default(),
        ),
        HighlightCondition::Flagged => RawMatches::Rows(
            actx.annotations
                .as_ref()
                .map(|a| a.flagged.clone())
                .unwrap_or_default(),
        ),
        HighlightCondition::Tagged { tag } => RawMatches::Rows(
            actx.annotations
                .as_ref()
                .and_then(|a| a.tagged.get(tag).cloned())
                .unwrap_or_default(),
        ),
    };
    Ok(project(raw, &rule.target, doc))
}

/// Absolute rows for a diagnostics condition: one issue when `issue_id` is
/// given, else the union of every row-filterable issue in the report. Stale
/// issues (no longer row-filterable against the current data) resolve to none.
fn diagnostic_rows(
    doc: &Document,
    report: &DiagnosticsReport,
    issue_id: Option<&str>,
) -> Vec<usize> {
    let collect = |id: &str, out: &mut Vec<usize>| {
        if let Ok(rows) = crate::diagnostics::issue_rows(doc, id) {
            out.extend(rows);
        }
    };
    let mut rows = Vec::new();
    match issue_id {
        Some(id) => collect(id, &mut rows),
        None => {
            for issue in report.source.iter().chain(report.current.iter()) {
                if issue.row_filterable {
                    collect(&issue.id, &mut rows);
                }
            }
        }
    }
    rows.sort_unstable();
    rows.dedup();
    rows
}

/// The data-revision component of a rule's cache key: scoped to the columns a
/// cell-predicate rule scans (so an edit elsewhere keeps it warm), the whole
/// document revision otherwise.
fn data_revision(rule: &HighlightRule, doc: &Document) -> u64 {
    if rule.is_cell_predicate() {
        let cols = predicate_columns(rule, doc);
        if !cols.is_empty() {
            return cols
                .iter()
                .map(|&c| doc.column_revision(c))
                .max()
                .unwrap_or_else(|| doc.revision());
        }
    }
    doc.revision()
}

// ---------------------------------------------------------------------------
// Windowed query + explain DTOs
// ---------------------------------------------------------------------------

/// One decorated cell in a window response (display coordinates).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaintedCell {
    /// Display (visible) row index.
    pub row: usize,
    pub col: usize,
    /// The winning rule for this cell.
    pub rule_id: String,
    pub decoration: Decoration,
}

/// The decorations for a bounded, already-priority-flattened window.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HighlightWindow {
    /// Document revision the matches were computed against.
    pub revision: u64,
    /// Display row the window starts at.
    pub start: usize,
    /// Decorated cells only (undecorated cells are omitted).
    pub cells: Vec<PaintedCell>,
}

/// One rule that matches a cell, in the explain response.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplainedRule {
    pub rule_id: String,
    pub name: String,
    pub priority: i32,
    pub decoration: Decoration,
    /// Whether this rule wins the cell (highest priority, id tie-break).
    pub winning: bool,
}

/// Every rule matching one cell, in winning order.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CellExplanation {
    pub row: usize,
    pub col: usize,
    pub rules: Vec<ExplainedRule>,
}

/// One matched (cell, rule) pair in an exported match report.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchReportEntry {
    /// Absolute row index.
    pub row: usize,
    pub col: usize,
    pub column_id: String,
    pub rule_id: String,
    pub rule_name: String,
    pub priority: i32,
    pub tone: SemanticTone,
    pub emphasis: Emphasis,
    /// Whether this rule wins the cell over the others matching it.
    pub winning: bool,
}

/// A complete match report for the chosen scope.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchReport {
    pub doc_revision: u64,
    pub entries: Vec<MatchReportEntry>,
}

/// Output format for the match report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportFormat {
    Json,
    Csv,
}

// ---------------------------------------------------------------------------
// Per-document state + cache
// ---------------------------------------------------------------------------

/// One active rule plus the monotonic revision of ITS definition (bumped only
/// when this rule changes, so editing one rule never invalidates another).
struct RuleEntry {
    rule: HighlightRule,
    revision: u64,
}

/// A rule's cached match set, valid while the triple key still matches.
struct CachedMatches {
    rule_revision: u64,
    data_revision: u64,
    analysis_revision: u64,
    matches: MatchSet,
}

/// One document's highlight state: the active rule set and each rule's cached
/// match set. Behind a per-document lock so a long recompute on one tab never
/// blocks highlighting on another.
#[derive(Default)]
pub struct DocState {
    entries: Vec<RuleEntry>,
    next_revision: u64,
    cache: HashMap<String, CachedMatches>,
}

impl DocState {
    fn next_rev(&mut self) -> u64 {
        self.next_revision += 1;
        self.next_revision
    }

    /// Replace the whole active rule set (e.g. loading a named view / profile).
    /// A rule whose definition is unchanged keeps its revision AND its cache;
    /// changed or new rules get a fresh revision; dropped rules lose their
    /// cache.
    fn set_rules(&mut self, rules: Vec<HighlightRule>) -> AppResult<()> {
        for rule in &rules {
            validate_rule(rule)?;
        }
        let mut old: HashMap<String, RuleEntry> = self
            .entries
            .drain(..)
            .map(|e| (e.rule.id.clone(), e))
            .collect();
        let mut new_entries: Vec<RuleEntry> = Vec::with_capacity(rules.len());
        for rule in rules {
            let revision = match old.remove(&rule.id) {
                Some(prev) if prev.rule == rule => prev.revision,
                _ => {
                    self.next_revision += 1;
                    self.next_revision
                }
            };
            new_entries.push(RuleEntry { rule, revision });
        }
        // Keep a cache entry only for a rule still present at the SAME revision
        // (built from a local map so the retain closure never touches `self`).
        let valid: HashMap<&str, u64> = new_entries
            .iter()
            .map(|e| (e.rule.id.as_str(), e.revision))
            .collect();
        self.cache
            .retain(|id, cached| valid.get(id.as_str()) == Some(&cached.rule_revision));
        drop(valid);
        self.entries = new_entries;
        Ok(())
    }

    /// Insert or replace ONE rule; bumps only its revision (and drops only its
    /// cache when the definition changed).
    fn upsert_rule(&mut self, rule: HighlightRule) -> AppResult<()> {
        validate_rule(&rule)?;
        match self.entries.iter().position(|e| e.rule.id == rule.id) {
            // Definition unchanged: a no-op that keeps the cached match set.
            Some(idx) if self.entries[idx].rule == rule => Ok(()),
            // Changed: fresh revision, and drop only this rule's cache.
            Some(idx) => {
                let revision = self.next_rev();
                self.cache.remove(&rule.id);
                self.entries[idx].rule = rule;
                self.entries[idx].revision = revision;
                Ok(())
            }
            None => {
                let revision = self.next_rev();
                self.entries.push(RuleEntry { rule, revision });
                Ok(())
            }
        }
    }

    /// Remove one rule (and its cache). Returns whether it existed.
    fn delete_rule(&mut self, rule_id: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| e.rule.id != rule_id);
        self.cache.remove(rule_id);
        self.entries.len() != before
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.cache.clear();
    }

    fn rules(&self) -> Vec<HighlightRule> {
        self.entries.iter().map(|e| e.rule.clone()).collect()
    }

    /// Ensure every enabled, NON-volatile rule has a valid cached match set.
    fn ensure_cached(&mut self, doc: &Document, actx: &AnalysisContext) -> AppResult<()> {
        // Snapshot the recompute plan first to avoid borrowing `self` twice.
        let mut recompute: Vec<(String, u64, u64, u64)> = Vec::new();
        for entry in &self.entries {
            if !entry.rule.enabled || entry.rule.is_volatile() {
                continue;
            }
            let data_rev = data_revision(&entry.rule, doc);
            let analysis_rev = analysis_revision(&entry.rule.condition, actx);
            let valid = self.cache.get(&entry.rule.id).is_some_and(|c| {
                c.rule_revision == entry.revision
                    && c.data_revision == data_rev
                    && c.analysis_revision == analysis_rev
            });
            if !valid {
                recompute.push((
                    entry.rule.id.clone(),
                    entry.revision,
                    data_rev,
                    analysis_rev,
                ));
            }
        }
        for (id, rule_revision, data_rev, analysis_rev) in recompute {
            let Some(rule) = self.entries.iter().find(|e| e.rule.id == id) else {
                continue;
            };
            let matches = evaluate_rule(&rule.rule, doc, actx)?;
            self.cache.insert(
                id,
                CachedMatches {
                    rule_revision,
                    data_revision: data_rev,
                    analysis_revision: analysis_rev,
                    matches,
                },
            );
        }
        Ok(())
    }

    /// Compute the volatile rules' match sets fresh (never cached).
    fn volatile_matches(
        &self,
        doc: &Document,
        actx: &AnalysisContext,
    ) -> AppResult<HashMap<String, MatchSet>> {
        let mut out = HashMap::new();
        for entry in &self.entries {
            if entry.rule.enabled && entry.rule.is_volatile() {
                out.insert(
                    entry.rule.id.clone(),
                    evaluate_rule(&entry.rule, doc, actx)?,
                );
            }
        }
        Ok(out)
    }

    /// Enabled rule indices in winning order: priority descending, id ascending.
    fn winning_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.entries.len())
            .filter(|&i| self.entries[i].rule.enabled)
            .collect();
        order.sort_by(|&a, &b| {
            let (ra, rb) = (&self.entries[a].rule, &self.entries[b].rule);
            rb.priority
                .cmp(&ra.priority)
                .then_with(|| ra.id.cmp(&rb.id))
        });
        order
    }

    /// Look up a rule's match set for the current query (volatile map or cache).
    fn matches_for<'a>(
        &'a self,
        id: &str,
        volatile: &'a HashMap<String, MatchSet>,
    ) -> Option<&'a MatchSet> {
        volatile
            .get(id)
            .or_else(|| self.cache.get(id).map(|c| &c.matches))
    }

    /// Resolve the decorations for a bounded display window, flattened by
    /// priority into one winning decoration per cell.
    pub fn window(
        &mut self,
        doc: &Document,
        actx: &AnalysisContext,
        start: usize,
        count: usize,
    ) -> AppResult<HighlightWindow> {
        if count > MAX_WINDOW_ROWS {
            return Err(AppError::invalid(format!(
                "highlight window of {count} rows exceeds the {MAX_WINDOW_ROWS}-row cap; \
                 fetch the visible range in bounded pages"
            )));
        }
        self.ensure_cached(doc, actx)?;
        let volatile = self.volatile_matches(doc, actx)?;

        let n_cols = doc.n_cols();
        let visible = doc.visible_len();
        let start = start.min(visible);
        let end = start.saturating_add(count).min(visible);
        let window_len = end - start;
        let revision = doc.revision();
        if window_len == 0 || n_cols == 0 {
            return Ok(HighlightWindow {
                revision,
                start,
                cells: Vec::new(),
            });
        }

        // Absolute row for each display row in the window.
        let abs: Vec<usize> = (start..end)
            .map(|d| doc.display_to_abs(d).unwrap_or(usize::MAX))
            .collect();

        // Claim each cell for the highest-priority rule that covers it.
        let order = self.winning_order();
        let mut claimed: Vec<Option<usize>> = vec![None; window_len * n_cols];
        for &entry_idx in &order {
            let id = &self.entries[entry_idx].rule.id;
            let Some(matches) = self.matches_for(id, &volatile) else {
                continue;
            };
            for (d_off, &a) in abs.iter().enumerate() {
                if a == usize::MAX {
                    continue;
                }
                match matches.coverage(a) {
                    RowCoverage::None => {}
                    RowCoverage::All => {
                        for c in 0..n_cols {
                            let slot = &mut claimed[d_off * n_cols + c];
                            if slot.is_none() {
                                *slot = Some(entry_idx);
                            }
                        }
                    }
                    RowCoverage::Cols(cols) => {
                        for &c in cols {
                            if c < n_cols {
                                let slot = &mut claimed[d_off * n_cols + c];
                                if slot.is_none() {
                                    *slot = Some(entry_idx);
                                }
                            }
                        }
                    }
                }
            }
        }

        let mut cells = Vec::new();
        for d_off in 0..window_len {
            for c in 0..n_cols {
                if let Some(entry_idx) = claimed[d_off * n_cols + c] {
                    let rule = &self.entries[entry_idx].rule;
                    cells.push(PaintedCell {
                        row: start + d_off,
                        col: c,
                        rule_id: rule.id.clone(),
                        decoration: rule.decoration.clone(),
                    });
                }
            }
        }
        Ok(HighlightWindow {
            revision,
            start,
            cells,
        })
    }

    /// Every rule matching one display cell, in winning order (first wins).
    pub fn explain(
        &mut self,
        doc: &Document,
        actx: &AnalysisContext,
        display_row: usize,
        col: usize,
    ) -> AppResult<CellExplanation> {
        if col >= doc.n_cols() {
            return Err(AppError::invalid("column index out of range"));
        }
        let abs = doc
            .display_to_abs(display_row)
            .ok_or_else(|| AppError::invalid("row index out of range"))?;
        self.ensure_cached(doc, actx)?;
        let volatile = self.volatile_matches(doc, actx)?;

        let mut rules: Vec<ExplainedRule> = Vec::new();
        for &entry_idx in &self.winning_order() {
            let rule = &self.entries[entry_idx].rule;
            let Some(matches) = self.matches_for(&rule.id, &volatile) else {
                continue;
            };
            if matches.matches_cell(abs, col) {
                rules.push(ExplainedRule {
                    rule_id: rule.id.clone(),
                    name: rule.name.clone(),
                    priority: rule.priority,
                    decoration: rule.decoration.clone(),
                    winning: false,
                });
            }
        }
        if let Some(first) = rules.first_mut() {
            first.winning = true;
        }
        Ok(CellExplanation {
            row: display_row,
            col,
            rules,
        })
    }

    /// Build a complete match report over `scope` (absolute coordinates). One
    /// entry per matched (cell, rule) pair, winning flag set on the top rule.
    ///
    /// When a [`JobCtx`] is supplied (the export command wraps this in a job),
    /// each scanned row advances it — streaming throttled progress AND observing
    /// cooperative cancellation. A `jobs.cancel` request aborts the scan with
    /// [`AppError::Cancelled`] instead of running to completion.
    pub fn report(
        &mut self,
        doc: &Document,
        actx: &AnalysisContext,
        scope: &ExportScope,
        ctx: Option<&JobCtx>,
    ) -> AppResult<MatchReport> {
        self.ensure_cached(doc, actx)?;
        let volatile = self.volatile_matches(doc, actx)?;
        let order = self.winning_order();
        let column_ids = doc.column_ids();
        let n_cols = doc.n_cols();

        let scope_rows = crate::export_scope::resolve_scope(doc, scope)?.rows;
        if let Some(ctx) = ctx {
            ctx.set_total(scope_rows.len() as u64);
        }
        let mut entries = Vec::new();
        for abs in scope_rows {
            // The report scans scope_rows × columns × rules; checkpoint once per
            // row so a large report stays cancellable and reports progress.
            if let Some(ctx) = ctx {
                ctx.advance(1)?;
            }
            for col in 0..n_cols {
                let mut winning = true;
                for &entry_idx in &order {
                    let rule = &self.entries[entry_idx].rule;
                    let Some(matches) = self.matches_for(&rule.id, &volatile) else {
                        continue;
                    };
                    if matches.matches_cell(abs, col) {
                        entries.push(MatchReportEntry {
                            row: abs,
                            col,
                            column_id: column_ids.get(col).cloned().unwrap_or_default(),
                            rule_id: rule.id.clone(),
                            rule_name: rule.name.clone(),
                            priority: rule.priority,
                            tone: rule.decoration.tone,
                            emphasis: rule.decoration.emphasis,
                            winning,
                        });
                        winning = false;
                    }
                }
            }
        }
        Ok(MatchReport {
            doc_revision: doc.revision(),
            entries,
        })
    }

    /// The live match count for every rule, in the store's order (id → count).
    /// Enabled rules read the warm window cache; disabled rules are evaluated
    /// fresh so the dialog can show a rule's impact before it is switched on.
    pub fn counts(
        &mut self,
        doc: &Document,
        actx: &AnalysisContext,
    ) -> AppResult<Vec<(String, usize)>> {
        self.ensure_cached(doc, actx)?;
        let volatile = self.volatile_matches(doc, actx)?;
        let mut out = Vec::with_capacity(self.entries.len());
        for entry in &self.entries {
            let count = if entry.rule.enabled {
                self.matches_for(&entry.rule.id, &volatile)
                    .map_or(0, MatchSet::count)
            } else {
                evaluate_rule(&entry.rule, doc, actx)?.count()
            };
            out.push((entry.rule.id.clone(), count));
        }
        Ok(out)
    }
}

/// Build a match report for `rules` against a document, independent of any
/// document's live cache. Used by the export command (job-wrapped): it stands
/// up a throwaway [`DocState`], so a report can be produced off the UI thread
/// without moving the shared store into the job.
pub fn build_report(
    rules: Vec<HighlightRule>,
    doc: &Document,
    actx: &AnalysisContext,
    scope: &ExportScope,
    ctx: Option<&JobCtx>,
) -> AppResult<MatchReport> {
    let mut state = DocState::default();
    state.set_rules(rules)?;
    state.report(doc, actx, scope, ctx)
}

/// Serialize a match report to the requested format.
pub fn serialize_report(report: &MatchReport, format: ReportFormat) -> AppResult<Vec<u8>> {
    match format {
        ReportFormat::Json => serde_json::to_vec_pretty(report)
            .map(|mut v| {
                v.push(b'\n');
                v
            })
            .map_err(|e| AppError::Other(format!("could not serialize match report: {e}"))),
        ReportFormat::Csv => {
            let mut out =
                String::from("row,col,columnId,ruleId,ruleName,priority,tone,emphasis,winning\n");
            for e in &report.entries {
                out.push_str(&format!(
                    "{},{},{},{},{},{},{},{},{}\n",
                    e.row,
                    e.col,
                    csv_field(&e.column_id),
                    csv_field(&e.rule_id),
                    csv_field(&e.rule_name),
                    e.priority,
                    tone_label(e.tone),
                    emphasis_label(e.emphasis),
                    e.winning,
                ));
            }
            Ok(out.into_bytes())
        }
    }
}

/// Minimal RFC 4180 CSV escaping for a single field.
fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn tone_label(tone: SemanticTone) -> &'static str {
    match tone {
        SemanticTone::Accent => "accent",
        SemanticTone::Info => "info",
        SemanticTone::Warn => "warn",
        SemanticTone::Error => "error",
        SemanticTone::Success => "success",
        SemanticTone::Neutral => "neutral",
    }
}

fn emphasis_label(emphasis: Emphasis) -> &'static str {
    match emphasis {
        Emphasis::Subtle => "subtle",
        Emphasis::Normal => "normal",
        Emphasis::Strong => "strong",
    }
}

// ---------------------------------------------------------------------------
// The store (Tauri-managed): per-document highlight state behind its own lock
// ---------------------------------------------------------------------------

/// A document's resolved F40 annotation match snapshot, tagged with the
/// (document, annotation-store) revisions it was built at so a rematch is only
/// paid when one of them moves — a plain scroll reuses the cached snapshot.
struct CachedAnnotationMatches {
    doc_revision: u64,
    annotation_revision: u64,
    matches: AnnotationMatches,
}

/// Process-wide highlight state, keyed by document id. The `states` map is
/// locked only long enough to clone a document's `Arc<Mutex<DocState>>`,
/// mirroring the per-tab independence of the document registry. The
/// `annotation_cache` memoizes the resolved F40 marks per document.
#[derive(Default)]
pub struct HighlightStore {
    states: Mutex<HashMap<u64, Arc<Mutex<DocState>>>>,
    annotation_cache: Mutex<HashMap<u64, CachedAnnotationMatches>>,
}

impl HighlightStore {
    /// The per-document state, created on first use.
    fn doc_state(&self, doc_id: u64) -> AppResult<Arc<Mutex<DocState>>> {
        let mut map = self
            .states
            .lock()
            .map_err(|_| AppError::Other("internal highlight lock error".into()))?;
        Ok(Arc::clone(map.entry(doc_id).or_default()))
    }

    /// The F40 annotation match snapshot for a document, resolved via `build`
    /// (a rematch against the annotation store) only when the document or the
    /// annotation-store revision has moved since the last resolve. Building is
    /// otherwise skipped, so a windowed scroll never re-runs the rematch.
    pub fn annotation_matches(
        &self,
        doc_id: u64,
        doc_revision: u64,
        annotation_revision: u64,
        build: impl FnOnce() -> AppResult<AnnotationMatches>,
    ) -> AppResult<AnnotationMatches> {
        let mut map = self
            .annotation_cache
            .lock()
            .map_err(|_| AppError::Other("internal highlight lock error".into()))?;
        if let Some(cached) = map.get(&doc_id) {
            if cached.doc_revision == doc_revision
                && cached.annotation_revision == annotation_revision
            {
                return Ok(cached.matches.clone());
            }
        }
        let matches = build()?;
        map.insert(
            doc_id,
            CachedAnnotationMatches {
                doc_revision,
                annotation_revision,
                matches: matches.clone(),
            },
        );
        Ok(matches)
    }

    fn with_state<T>(&self, doc_id: u64, f: impl FnOnce(&mut DocState) -> T) -> AppResult<T> {
        let state = self.doc_state(doc_id)?;
        let mut guard = state
            .lock()
            .map_err(|_| AppError::Other("internal highlight lock error".into()))?;
        Ok(f(&mut guard))
    }

    /// The active rules for a document.
    pub fn list_rules(&self, doc_id: u64) -> AppResult<Vec<HighlightRule>> {
        self.with_state(doc_id, |s| s.rules())
    }

    /// Replace the whole active rule set (targeted revision bumping inside).
    pub fn set_rules(&self, doc_id: u64, rules: Vec<HighlightRule>) -> AppResult<()> {
        self.with_state(doc_id, |s| s.set_rules(rules))?
    }

    /// Insert or replace one rule.
    pub fn upsert_rule(&self, doc_id: u64, rule: HighlightRule) -> AppResult<()> {
        self.with_state(doc_id, |s| s.upsert_rule(rule))?
    }

    /// Delete one rule.
    pub fn delete_rule(&self, doc_id: u64, rule_id: &str) -> AppResult<bool> {
        self.with_state(doc_id, |s| s.delete_rule(rule_id))
    }

    /// Clear every rule for a document.
    pub fn clear(&self, doc_id: u64) -> AppResult<()> {
        self.with_state(doc_id, |s| s.clear())
    }

    /// Forget a document entirely (on close): its rule state and its cached
    /// annotation snapshot.
    pub fn remove_doc(&self, doc_id: u64) {
        if let Ok(mut map) = self.states.lock() {
            map.remove(&doc_id);
        }
        if let Ok(mut map) = self.annotation_cache.lock() {
            map.remove(&doc_id);
        }
    }

    /// Resolve one window against a document + its analysis context.
    pub fn window(
        &self,
        doc_id: u64,
        doc: &Document,
        actx: &AnalysisContext,
        start: usize,
        count: usize,
    ) -> AppResult<HighlightWindow> {
        self.with_state(doc_id, |s| s.window(doc, actx, start, count))?
    }

    /// Explain one cell.
    pub fn explain(
        &self,
        doc_id: u64,
        doc: &Document,
        actx: &AnalysisContext,
        display_row: usize,
        col: usize,
    ) -> AppResult<CellExplanation> {
        self.with_state(doc_id, |s| s.explain(doc, actx, display_row, col))?
    }

    /// Build a match report.
    pub fn report(
        &self,
        doc_id: u64,
        doc: &Document,
        actx: &AnalysisContext,
        scope: &ExportScope,
    ) -> AppResult<MatchReport> {
        self.with_state(doc_id, |s| s.report(doc, actx, scope, None))?
    }

    /// Per-rule live match counts (rule id → count), in store order.
    pub fn counts(
        &self,
        doc_id: u64,
        doc: &Document,
        actx: &AnalysisContext,
    ) -> AppResult<Vec<(String, usize)>> {
        self.with_state(doc_id, |s| s.counts(doc, actx))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse, ParseSettings};

    fn doc(csv: &str) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, true)
    }

    fn rule(id: &str, condition: HighlightCondition, target: HighlightTarget) -> HighlightRule {
        HighlightRule {
            id: id.into(),
            name: id.into(),
            condition,
            target,
            priority: 0,
            decoration: Decoration::default(),
            enabled: true,
        }
    }

    fn col_id(doc: &Document, col: usize) -> String {
        doc.column_ids()[col].clone()
    }

    /// Evaluate one rule directly (no cache) and return its match set.
    fn eval(d: &Document, r: &HighlightRule) -> MatchSet {
        evaluate_rule(r, d, &AnalysisContext::default()).unwrap()
    }

    fn cells_of(m: &MatchSet) -> Vec<(usize, usize)> {
        match m {
            MatchSet::Cells(map) => {
                let mut out: Vec<(usize, usize)> = map
                    .iter()
                    .flat_map(|(&r, cols)| cols.iter().map(move |&c| (r, c)))
                    .collect();
                out.sort_unstable();
                out
            }
            MatchSet::Rows(rows) => rows.iter().map(|&r| (r, usize::MAX)).collect(),
        }
    }

    fn rows_of(m: &MatchSet) -> Vec<usize> {
        match m {
            MatchSet::Rows(rows) => rows.clone(),
            MatchSet::Cells(map) => {
                let mut r: Vec<usize> = map.keys().copied().collect();
                r.sort_unstable();
                r
            }
        }
    }

    // ----- per-cell predicates -------------------------------------------

    #[test]
    fn equals_and_not_equals_are_trimmed_and_case_aware() {
        let d = doc("a,b\napple, x \nAPPLE,y\nbanana,z");
        let c0 = col_id(&d, 0);
        let eq = rule(
            "r",
            HighlightCondition::Equals {
                column_id: Some(c0.clone()),
                value: "apple".into(),
                case_sensitive: false,
            },
            HighlightTarget::Cell,
        );
        assert_eq!(cells_of(&eval(&d, &eq)), vec![(0, 0), (1, 0)]);

        let eq_cs = rule(
            "r",
            HighlightCondition::Equals {
                column_id: Some(c0.clone()),
                value: "apple".into(),
                case_sensitive: true,
            },
            HighlightTarget::Cell,
        );
        assert_eq!(cells_of(&eval(&d, &eq_cs)), vec![(0, 0)]);

        let ne = rule(
            "r",
            HighlightCondition::NotEquals {
                column_id: Some(c0),
                value: "apple".into(),
                case_sensitive: false,
            },
            HighlightTarget::Cell,
        );
        assert_eq!(cells_of(&eval(&d, &ne)), vec![(2, 0)]);
    }

    #[test]
    fn contains_and_regex() {
        let d = doc("s\nfoo123\nbar\nBaz9");
        let c = col_id(&d, 0);
        let contains = rule(
            "r",
            HighlightCondition::Contains {
                column_id: Some(c.clone()),
                value: "ba".into(),
                case_sensitive: false,
            },
            HighlightTarget::Cell,
        );
        assert_eq!(cells_of(&eval(&d, &contains)), vec![(1, 0), (2, 0)]);

        let re = rule(
            "r",
            HighlightCondition::Regex {
                column_id: Some(c),
                pattern: r"\d+".into(),
                case_sensitive: false,
            },
            HighlightTarget::Cell,
        );
        assert_eq!(cells_of(&eval(&d, &re)), vec![(0, 0), (2, 0)]);
    }

    #[test]
    fn numeric_and_date_ranges() {
        let d = doc("n\n5\n10\n15\nx");
        let c = col_id(&d, 0);
        let num = rule(
            "r",
            HighlightCondition::NumericRange {
                column_id: Some(c),
                min: Some(6.0),
                max: Some(12.0),
                inclusive: true,
            },
            HighlightTarget::Cell,
        );
        // 10 matches; 5 and 15 out; "x" non-numeric.
        assert_eq!(cells_of(&eval(&d, &num)), vec![(1, 0)]);

        let dd = doc("d\n2024-01-01\n2024-06-15\n2025-01-01\n");
        let dc = col_id(&dd, 0);
        let date = rule(
            "r",
            HighlightCondition::DateRange {
                column_id: Some(dc),
                min: Some("2024-02-01".into()),
                max: Some("2024-12-31".into()),
            },
            HighlightTarget::Cell,
        );
        assert_eq!(cells_of(&eval(&dd, &date)), vec![(1, 0)]);
    }

    #[test]
    fn numeric_range_exclusive_bounds() {
        let d = doc("n\n5\n10\n15");
        let c = col_id(&d, 0);
        let excl = rule(
            "r",
            HighlightCondition::NumericRange {
                column_id: Some(c),
                min: Some(5.0),
                max: Some(15.0),
                inclusive: false,
            },
            HighlightTarget::Cell,
        );
        assert_eq!(cells_of(&eval(&d, &excl)), vec![(1, 0)]);
    }

    #[test]
    fn blank_uses_schema_null_tokens() {
        let mut d = doc("v\n \nNULL\nx\n");
        let c = col_id(&d, 0);
        // Without a schema, only the whitespace-only cell is blank.
        let blank = rule(
            "r",
            HighlightCondition::Blank {
                column_id: Some(c.clone()),
            },
            HighlightTarget::Cell,
        );
        assert_eq!(cells_of(&eval(&d, &blank)), vec![(0, 0)]);

        // Declaring NULL as a null token makes row 1 blank too.
        let mut schema = ColumnSchema::new(c.clone(), "v", LogicalType::Text);
        schema.null_tokens = vec!["NULL".into()];
        d.set_column_schema(schema);
        assert_eq!(cells_of(&eval(&d, &blank)), vec![(0, 0), (1, 0)]);
    }

    #[test]
    fn invalid_needs_a_declared_type() {
        let mut d = doc("n\n1\nabc\n2\n");
        let c = col_id(&d, 0);
        let invalid = rule(
            "r",
            HighlightCondition::Invalid {
                column_id: Some(c.clone()),
            },
            HighlightTarget::Cell,
        );
        // No declared type: nothing is "invalid".
        assert!(cells_of(&eval(&d, &invalid)).is_empty());
        d.set_column_schema(ColumnSchema::new(c, "n", LogicalType::Integer));
        assert_eq!(cells_of(&eval(&d, &invalid)), vec![(1, 0)]);
    }

    #[test]
    fn duplicate_values_reuse_dedup_and_skip_blanks() {
        // A second column keeps every row non-empty so the parser preserves the
        // blank KEY cells (rows 3,4) — a fully blank line would be dropped.
        let d = doc("k,x\na,1\nb,2\na,3\n,4\n,5\nB,6");
        let c = col_id(&d, 0);
        let dup = rule(
            "r",
            HighlightCondition::Duplicate {
                column_id: Some(c.clone()),
                trim: false,
                case_insensitive: false,
                collapse_whitespace: false,
            },
            HighlightTarget::Cell,
        );
        // "a" at rows 0 and 2 duplicate; blank keys (rows 3,4) never count.
        assert_eq!(cells_of(&eval(&d, &dup)), vec![(0, 0), (2, 0)]);

        // Case-insensitive additionally unifies "b"/"B".
        let dup_ci = rule(
            "r",
            HighlightCondition::Duplicate {
                column_id: Some(c),
                trim: false,
                case_insensitive: true,
                collapse_whitespace: false,
            },
            HighlightTarget::Cell,
        );
        assert_eq!(
            cells_of(&eval(&d, &dup_ci)),
            vec![(0, 0), (1, 0), (2, 0), (5, 0)]
        );
    }

    #[test]
    fn changed_since_save_reads_dirty_cells() {
        let mut d = doc("a,b\nx,y\np,q");
        d.set_cell(1, 0, "P".into()).unwrap();
        let changed = rule(
            "r",
            HighlightCondition::ChangedSinceSave { column_id: None },
            HighlightTarget::Cell,
        );
        assert_eq!(cells_of(&eval(&d, &changed)), vec![(1, 0)]);
        // A save clears the dirty set → no matches (volatile, recomputed).
        d.mark_saved(None);
        assert!(cells_of(&eval(&d, &changed)).is_empty());
    }

    // ----- targets --------------------------------------------------------

    #[test]
    fn row_target_decorates_whole_rows() {
        let d = doc("a,b\nhit,1\nno,2\nhit,3");
        let c0 = col_id(&d, 0);
        let r = rule(
            "r",
            HighlightCondition::Equals {
                column_id: Some(c0),
                value: "hit".into(),
                case_sensitive: false,
            },
            HighlightTarget::Row,
        );
        assert_eq!(rows_of(&eval(&d, &r)), vec![0, 2]);
    }

    #[test]
    fn columns_target_scopes_and_expands() {
        let d = doc("a,b,c\nx,x,x\ny,x,y");
        let (c0, c1, c2) = (col_id(&d, 0), col_id(&d, 1), col_id(&d, 2));
        // Condition with no column: scoped to the target columns a & c.
        let r = rule(
            "r",
            HighlightCondition::Equals {
                column_id: None,
                value: "x".into(),
                case_sensitive: false,
            },
            HighlightTarget::Columns {
                column_ids: vec![c0, c2],
            },
        );
        // Row 0: a=x, c=x → (0,0),(0,2); row 1: c=y no, a=y no.
        assert_eq!(cells_of(&eval(&d, &r)), vec![(0, 0), (0, 2)]);
        let _ = c1;
    }

    // ----- analysis-backed conditions ------------------------------------

    #[test]
    fn outlier_condition_consumes_cached_spec() {
        use crate::outlier::{OutlierMethod, OutlierReport, OutlierSpec};
        let mut csv = String::from("n,x\n");
        for i in 1..=9 {
            csv.push_str(&format!("{i},1\n"));
        }
        csv.push_str("100,1\n");
        let d = doc(&csv);
        let spec = OutlierSpec {
            column: 0,
            method: OutlierMethod::Iqr { k: 1.5 },
            group_columns: vec![],
            scope: ExportScope::All,
        };
        let report = OutlierReport {
            revision: d.revision(),
            scanned_rows: 10,
            considered: 10,
            flagged: 1,
            blanks: 0,
            invalid_numeric: 0,
            groups: vec![],
            groups_total: 1,
            sample: vec![],
        };
        let actx = AnalysisContext {
            outlier: Some((spec, report)),
            ..Default::default()
        };
        let r = rule("r", HighlightCondition::Outlier, HighlightTarget::Cell);
        let m = evaluate_rule(&r, &d, &actx).unwrap();
        assert_eq!(cells_of(&m), vec![(9, 0)]);

        // No cache → no matches.
        let empty = evaluate_rule(&r, &d, &AnalysisContext::default()).unwrap();
        assert!(rows_of(&empty).is_empty());
    }

    #[test]
    fn cross_column_condition_consumes_cached_rules() {
        use crate::crossval::{CrossRule, CrossValReport};
        let d = doc("a,b\nx,x\nx,y\n1,2");
        let rules = vec![CrossRule::ColumnsEqual {
            left: "a".into(),
            right: "b".into(),
            negate: false,
        }];
        let report = CrossValReport {
            revision: d.revision(),
            scanned_rows: 3,
            total_violations: 2,
            violating_rows: 2,
            rules: vec![],
        };
        let actx = AnalysisContext {
            crossval: Some((rules, report)),
            ..Default::default()
        };
        let r = rule(
            "r",
            HighlightCondition::CrossColumn { rule_index: None },
            HighlightTarget::Row,
        );
        assert_eq!(rows_of(&evaluate_rule(&r, &d, &actx).unwrap()), vec![1, 2]);
    }

    fn annotations_ctx(
        revision: u64,
        starred: Vec<usize>,
        flagged: Vec<usize>,
        tagged: &[(&str, &[usize])],
    ) -> AnalysisContext {
        AnalysisContext {
            annotations: Some(AnnotationMatches {
                revision,
                starred,
                flagged,
                tagged: tagged
                    .iter()
                    .map(|(t, rows)| (t.to_string(), rows.to_vec()))
                    .collect(),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn annotation_conditions_decorate_marked_rows() {
        let d = doc("a\n1\n2\n3\n4");
        let actx = annotations_ctx(7, vec![0, 2], vec![3], &[("keep", &[1, 3])]);

        let bm = rule("bm", HighlightCondition::Bookmarked, HighlightTarget::Row);
        assert_eq!(rows_of(&evaluate_rule(&bm, &d, &actx).unwrap()), vec![0, 2]);

        let fl = rule("fl", HighlightCondition::Flagged, HighlightTarget::Row);
        assert_eq!(rows_of(&evaluate_rule(&fl, &d, &actx).unwrap()), vec![3]);

        let tg = rule(
            "tg",
            HighlightCondition::Tagged { tag: "keep".into() },
            HighlightTarget::Row,
        );
        assert_eq!(rows_of(&evaluate_rule(&tg, &d, &actx).unwrap()), vec![1, 3]);

        // An unknown tag (and any condition with no annotation context) is empty.
        let missing = rule(
            "missing",
            HighlightCondition::Tagged {
                tag: "absent".into(),
            },
            HighlightTarget::Row,
        );
        assert!(rows_of(&evaluate_rule(&missing, &d, &actx).unwrap()).is_empty());
    }

    #[test]
    fn annotation_conditions_empty_without_context() {
        let d = doc("a\n1\n2");
        for cond in [
            HighlightCondition::Bookmarked,
            HighlightCondition::Flagged,
            HighlightCondition::Tagged { tag: "t".into() },
        ] {
            let r = rule("r", cond, HighlightTarget::Row);
            assert!(rows_of(&eval(&d, &r)).is_empty());
        }
    }

    #[test]
    fn annotation_condition_caches_and_invalidates_on_revision() {
        let d = doc("a\n1\n2\n3");
        let mut state = DocState::default();
        state
            .set_rules(vec![rule(
                "bm",
                HighlightCondition::Bookmarked,
                HighlightTarget::Row,
            )])
            .unwrap();
        let painted = |w: &HighlightWindow| w.cells.iter().map(|c| c.row).collect::<Vec<_>>();

        // Row 0 starred at annotation revision 1.
        let w = state
            .window(&d, &annotations_ctx(1, vec![0], vec![], &[]), 0, 3)
            .unwrap();
        assert_eq!(painted(&w), vec![0]);

        // A different snapshot at the SAME revision serves the warm cache — the
        // annotation condition is cached (not recomputed) until the revision moves.
        let w = state
            .window(&d, &annotations_ctx(1, vec![2], vec![], &[]), 0, 3)
            .unwrap();
        assert_eq!(painted(&w), vec![0]);

        // Bumping the annotation revision invalidates the cache → new rows win.
        let w = state
            .window(&d, &annotations_ctx(2, vec![2], vec![], &[]), 0, 3)
            .unwrap();
        assert_eq!(painted(&w), vec![2]);
    }

    // ----- windowed query + priority flattening --------------------------

    #[test]
    fn window_flattens_overlap_by_priority() {
        let d = doc("a,b\nx,1\ny,2\nx,3");
        let c0 = col_id(&d, 0);
        let store = HighlightStore::default();
        // Low-priority row rule paints whole rows; high-priority cell rule wins
        // the (0,0) & (2,0) cells.
        let row_rule = HighlightRule {
            priority: 1,
            decoration: Decoration {
                tone: SemanticTone::Info,
                ..Decoration::default()
            },
            ..rule(
                "row",
                HighlightCondition::Equals {
                    column_id: Some(c0.clone()),
                    value: "x".into(),
                    case_sensitive: false,
                },
                HighlightTarget::Row,
            )
        };
        let cell_rule = HighlightRule {
            priority: 5,
            decoration: Decoration {
                tone: SemanticTone::Error,
                ..Decoration::default()
            },
            ..rule(
                "cell",
                HighlightCondition::Equals {
                    column_id: Some(c0),
                    value: "x".into(),
                    case_sensitive: false,
                },
                HighlightTarget::Cell,
            )
        };
        store.set_rules(1, vec![row_rule, cell_rule]).unwrap();
        let w = store
            .window(1, &d, &AnalysisContext::default(), 0, 10)
            .unwrap();
        // Row 0: (0,0) → cell rule (error), (0,1) → row rule (info).
        let at = |row, col| w.cells.iter().find(|p| p.row == row && p.col == col);
        assert_eq!(at(0, 0).unwrap().rule_id, "cell");
        assert_eq!(at(0, 1).unwrap().rule_id, "row");
        assert_eq!(at(2, 0).unwrap().rule_id, "cell");
        assert!(at(1, 0).is_none(), "row 1 has no match");
    }

    #[test]
    fn window_respects_filter_view_and_display_coords() {
        let mut d = doc("a\nhit\nno\nhit\nno");
        d.set_filter(vec![0, 2]).unwrap(); // only the two "hit" rows visible
        let c0 = col_id(&d, 0);
        let store = HighlightStore::default();
        store
            .set_rules(
                1,
                vec![rule(
                    "r",
                    HighlightCondition::Equals {
                        column_id: Some(c0),
                        value: "hit".into(),
                        case_sensitive: false,
                    },
                    HighlightTarget::Cell,
                )],
            )
            .unwrap();
        let w = store
            .window(1, &d, &AnalysisContext::default(), 0, 10)
            .unwrap();
        // Both visible rows match; reported in DISPLAY coordinates 0 and 1.
        assert_eq!(w.cells.len(), 2);
        assert_eq!(w.cells[0].row, 0);
        assert_eq!(w.cells[1].row, 1);
    }

    #[test]
    fn window_rejects_oversized_requests() {
        let d = doc("a\n1");
        let store = HighlightStore::default();
        assert!(store
            .window(1, &d, &AnalysisContext::default(), 0, MAX_WINDOW_ROWS + 1)
            .is_err());
    }

    // ----- targeted cache invalidation -----------------------------------

    #[test]
    fn editing_one_rule_leaves_the_others_cache() {
        let d = doc("a,b\nx,y");
        let c0 = col_id(&d, 0);
        let c1 = col_id(&d, 1);
        let store = HighlightStore::default();
        store
            .set_rules(
                1,
                vec![
                    rule(
                        "A",
                        HighlightCondition::Equals {
                            column_id: Some(c0),
                            value: "x".into(),
                            case_sensitive: false,
                        },
                        HighlightTarget::Cell,
                    ),
                    rule(
                        "B",
                        HighlightCondition::Equals {
                            column_id: Some(c1.clone()),
                            value: "y".into(),
                            case_sensitive: false,
                        },
                        HighlightTarget::Cell,
                    ),
                ],
            )
            .unwrap();
        // Prime the cache.
        store
            .window(1, &d, &AnalysisContext::default(), 0, 10)
            .unwrap();

        // Capture B's cached revision, edit A, assert B's cache survived.
        let state = store.doc_state(1).unwrap();
        let b_rev_before = {
            let s = state.lock().unwrap();
            s.cache.get("B").unwrap().rule_revision
        };
        store
            .upsert_rule(
                1,
                rule(
                    "A",
                    HighlightCondition::Equals {
                        column_id: Some(c1),
                        value: "y".into(),
                        case_sensitive: false,
                    },
                    HighlightTarget::Cell,
                ),
            )
            .unwrap();
        {
            let s = state.lock().unwrap();
            assert!(
                s.cache.contains_key("B"),
                "B's cache must survive an A edit"
            );
            assert_eq!(
                s.cache.get("B").unwrap().rule_revision,
                b_rev_before,
                "B's revision must be unchanged"
            );
            assert!(!s.cache.contains_key("A"), "A's cache is dropped on edit");
        }
    }

    #[test]
    fn editing_unrelated_column_keeps_column_scoped_cache() {
        let mut d = doc("a,b\nx,y\nx,y");
        let c0 = col_id(&d, 0);
        let store = HighlightStore::default();
        store
            .set_rules(
                1,
                vec![rule(
                    "A",
                    HighlightCondition::Equals {
                        column_id: Some(c0),
                        value: "x".into(),
                        case_sensitive: false,
                    },
                    HighlightTarget::Cell,
                )],
            )
            .unwrap();
        store
            .window(1, &d, &AnalysisContext::default(), 0, 10)
            .unwrap();
        let data_rev_before = {
            let s = store.doc_state(1).unwrap();
            let g = s.lock().unwrap();
            g.cache.get("A").unwrap().data_revision
        };
        // Edit column b (index 1) — rule A only scans column a.
        d.set_cell(0, 1, "Z".into()).unwrap();
        let r = rule(
            "A",
            HighlightCondition::Equals {
                column_id: Some(col_id(&d, 0)),
                value: "x".into(),
                case_sensitive: false,
            },
            HighlightTarget::Cell,
        );
        assert_eq!(
            data_revision(&r, &d),
            data_rev_before,
            "column a's revision is unchanged by a column b edit"
        );
    }

    // ----- explain --------------------------------------------------------

    #[test]
    fn explain_lists_rules_in_priority_order() {
        let d = doc("a\nx");
        let c0 = col_id(&d, 0);
        let store = HighlightStore::default();
        let mk = |id: &str, prio: i32| HighlightRule {
            priority: prio,
            ..rule(
                id,
                HighlightCondition::Equals {
                    column_id: Some(c0.clone()),
                    value: "x".into(),
                    case_sensitive: false,
                },
                HighlightTarget::Cell,
            )
        };
        store
            .set_rules(1, vec![mk("low", 1), mk("high", 9)])
            .unwrap();
        let e = store
            .explain(1, &d, &AnalysisContext::default(), 0, 0)
            .unwrap();
        assert_eq!(e.rules.len(), 2);
        assert_eq!(e.rules[0].rule_id, "high");
        assert!(e.rules[0].winning);
        assert!(!e.rules[1].winning);
    }

    // ----- report export --------------------------------------------------

    #[test]
    fn report_export_json_and_csv_round_trip() {
        let d = doc("a,b\nx,1\ny,2");
        let c0 = col_id(&d, 0);
        let store = HighlightStore::default();
        store
            .set_rules(
                1,
                vec![rule(
                    "r",
                    HighlightCondition::Equals {
                        column_id: Some(c0),
                        value: "x".into(),
                        case_sensitive: false,
                    },
                    HighlightTarget::Cell,
                )],
            )
            .unwrap();
        let report = store
            .report(1, &d, &AnalysisContext::default(), &ExportScope::All)
            .unwrap();
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].row, 0);
        assert!(report.entries[0].winning);

        let json = serialize_report(&report, ReportFormat::Json).unwrap();
        assert!(std::str::from_utf8(&json).unwrap().contains("\"ruleId\""));
        let csv = serialize_report(&report, ReportFormat::Csv).unwrap();
        let csv = std::str::from_utf8(&csv).unwrap();
        assert!(csv.starts_with("row,col,columnId"));
        // The single data row carries row/col plus the matching rule's id and
        // name (row,col,columnId,ruleId,ruleName,…) — and never the cell value.
        assert!(
            csv.lines()
                .nth(1)
                .is_some_and(|l| l.starts_with("0,0,") && l.contains(",r,r,")),
            "expected one data row referencing rule r, got: {csv}"
        );
    }

    #[test]
    fn report_job_reports_progress_and_honors_cancellation() {
        use crate::job::{JobEvent, JobRegistry};
        use std::sync::{Arc, Mutex};

        let d = doc("a,b\nx,1\ny,2\nz,3");
        let c0 = col_id(&d, 0);
        let rules = || {
            vec![rule(
                "r",
                HighlightCondition::Equals {
                    column_id: Some(c0.clone()),
                    value: "x".into(),
                    case_sensitive: false,
                },
                HighlightTarget::Cell,
            )]
        };
        let registry = JobRegistry::default();

        // A live (uncancelled) job produces the same entries as the un-jobbed
        // path and sizes the job's progress bar to the scanned row count via
        // set_total (per-row advances are throttled, so their emitted count is
        // timing-dependent and not asserted here — the cancellation half below
        // proves the row loop actually calls advance).
        let events: Arc<Mutex<Vec<JobEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        let ctx = registry.begin("export", None, move |e| sink.lock().unwrap().push(e));
        let ok = build_report(
            rules(),
            &d,
            &AnalysisContext::default(),
            &ExportScope::All,
            Some(&ctx),
        )
        .unwrap();
        assert_eq!(ok.entries.len(), 1);
        let reported_total = events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, JobEvent::Progress(p) if p.total == Some(3)));
        assert!(reported_total, "set_total reports the three scanned rows");

        // Cancelling before the scan runs: the row loop observes it on the very
        // first row and aborts instead of running to completion.
        let ctx = registry.begin("export", None, |_| {});
        assert!(registry.cancel(ctx.id));
        let err = build_report(
            rules(),
            &d,
            &AnalysisContext::default(),
            &ExportScope::All,
            Some(&ctx),
        )
        .unwrap_err();
        assert!(matches!(err, AppError::Cancelled));
    }

    // ----- validation -----------------------------------------------------

    #[test]
    fn validation_surfaces_regex_and_range_errors() {
        let bad_regex = rule(
            "r",
            HighlightCondition::Regex {
                column_id: None,
                pattern: "([".into(),
                case_sensitive: false,
            },
            HighlightTarget::Cell,
        );
        assert!(validate_rule(&bad_regex).is_err());

        let bad_range = rule(
            "r",
            HighlightCondition::NumericRange {
                column_id: None,
                min: Some(10.0),
                max: Some(1.0),
                inclusive: true,
            },
            HighlightTarget::Cell,
        );
        assert!(validate_rule(&bad_range).is_err());

        let empty_cols = rule(
            "r",
            HighlightCondition::Blank { column_id: None },
            HighlightTarget::Columns { column_ids: vec![] },
        );
        assert!(validate_rule(&empty_cols).is_err());
    }

    // ----- persistence: rules carry no cell data -------------------------

    #[test]
    fn serialized_rule_has_no_forbidden_project_keys() {
        // A project's no-cell-data guard rejects these keys anywhere in a
        // section; a persisted rule must never serialize under them.
        let r = HighlightRule {
            id: "r".into(),
            name: "demo".into(),
            condition: HighlightCondition::Equals {
                column_id: Some("c0".into()),
                value: "secret-cell-content".into(),
                case_sensitive: true,
            },
            target: HighlightTarget::Columns {
                column_ids: vec!["c0".into()],
            },
            priority: 3,
            decoration: Decoration {
                tone: SemanticTone::Warn,
                emphasis: Emphasis::Strong,
                icon: Some("flag".into()),
                text_style: TextStyle::Bold,
            },
            enabled: true,
        };
        let json = serde_json::to_string(&r).unwrap();
        for forbidden in [
            "\"cells\"",
            "\"cellValues\"",
            "\"cellData\"",
            "\"rows\"",
            "\"rowData\"",
            "\"records\"",
            "\"values\"",
            "\"samples\"",
            "\"sampleRows\"",
            "\"clipboard\"",
        ] {
            assert!(
                !json.contains(forbidden),
                "rule JSON must not contain {forbidden}: {json}"
            );
        }
        // Round-trips unchanged.
        let back: HighlightRule = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn set_rules_preserves_cache_for_unchanged_rules() {
        let d = doc("a\nx");
        let c0 = col_id(&d, 0);
        let store = HighlightStore::default();
        let a = rule(
            "A",
            HighlightCondition::Equals {
                column_id: Some(c0.clone()),
                value: "x".into(),
                case_sensitive: false,
            },
            HighlightTarget::Cell,
        );
        store.set_rules(1, vec![a.clone()]).unwrap();
        store
            .window(1, &d, &AnalysisContext::default(), 0, 10)
            .unwrap();
        // Re-set with the same rule plus a new one; A's cache survives.
        let b = rule(
            "B",
            HighlightCondition::Blank {
                column_id: Some(c0),
            },
            HighlightTarget::Cell,
        );
        store.set_rules(1, vec![a, b]).unwrap();
        let s = store.doc_state(1).unwrap();
        let g = s.lock().unwrap();
        assert!(g.cache.contains_key("A"), "unchanged rule keeps its cache");
    }

    // ----- per-rule counts ------------------------------------------------

    #[test]
    fn counts_report_per_rule_totals_including_disabled() {
        let d = doc("a,b\nx,1\ny,2\nx,3");
        let c0 = col_id(&d, 0);
        let store = HighlightStore::default();
        // An enabled cell rule matching "x" (2 cells) and a DISABLED row rule
        // matching "x" (2 rows) — the disabled rule still reports its impact.
        let cell_rule = rule(
            "cell",
            HighlightCondition::Equals {
                column_id: Some(c0.clone()),
                value: "x".into(),
                case_sensitive: false,
            },
            HighlightTarget::Cell,
        );
        let row_rule = HighlightRule {
            enabled: false,
            ..rule(
                "row",
                HighlightCondition::Equals {
                    column_id: Some(c0),
                    value: "x".into(),
                    case_sensitive: false,
                },
                HighlightTarget::Row,
            )
        };
        store.set_rules(1, vec![cell_rule, row_rule]).unwrap();
        let counts = store.counts(1, &d, &AnalysisContext::default()).unwrap();
        assert_eq!(
            counts,
            vec![("cell".to_string(), 2), ("row".to_string(), 2)]
        );
    }
}
