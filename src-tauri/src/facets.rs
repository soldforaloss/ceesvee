//! Multi-facet exploration engine (F39).
//!
//! A *facet* is one dimension of a document the user can slice by. Several
//! facets are active at once; the population is the AND across facet panels and
//! the OR among the selected values inside one panel, with a per-value
//! include/exclude mode. Each facet's bucket counts are recomputed against the
//! population filtered by *all the other* facets — classic faceted search — so
//! selecting a city updates the age histogram but not the city counts.
//!
//! # Design
//!
//! The whole engine is a **pure function of a [`Document`] plus a
//! [`FacetInputs`] bundle**, so it is entirely unit-testable and never touches
//! Tauri state. Value facets (text / number / date / boolean / nullability /
//! semantic) read cell values (and, when a column has a declared F31 schema,
//! classify through it). The four *status* facets (diagnostics, validation,
//! duplicate, annotation) are row-level: their per-row membership is resolved by
//! the command layer from the live analysis caches (or recomputed) and handed in
//! as [`FacetInputs`], mirroring how F42 highlighting snapshots caches into an
//! `AnalysisContext`. Convenience constructors ([`StatusInput::from_marks`],
//! [`StatusInput::from_diagnostics`], …) build those from the standard report
//! types so the command layer stays trivial.
//!
//! # Cross-filter counting
//!
//! [`compute`] makes a single streaming pass. For each row it evaluates every
//! facet's selection predicate and counts the failures. A row joins the final
//! population when **no** facet rejects it; a facet's own counts include a row
//! when **every other** facet accepts it (i.e. either nothing fails, or the only
//! failing facet is this one). That is the standard faceting optimisation and
//! yields exact "counts reflect all other facets' selections" semantics in one
//! pass.
//!
//! # Non-destructive integration
//!
//! Applying facets is [`matching_rows`]: an exact full-document scan returning
//! the absolute row indices the current selection admits, ready to hand to
//! [`Document::set_filter`]. It produces the same kind of row view a plain
//! filter does, so the F12 view sort, scoped export and visible-row export all
//! compose for free. Computing counts and applying facets never mutate the
//! document and never dirty it. [`to_filter_group`] converts the (convertible)
//! facets to the standard filter-builder representation — a deliberately
//! one-way, lossy conversion (semantic / status facets have no column-filter
//! equivalent and are reported as dropped).
//!
//! Counts may be estimated from a leading sample on very large indexed
//! documents ([`FacetResultSet::sampled`] flags it); the *applied* filter is
//! always exact over the whole document.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::analyze;
use crate::annotations::MarkIndex;
use crate::crossval::{self, CrossRule};
use crate::dedup::{self, DedupSpec};
use crate::diagnostics::{self, DiagnosticsReport};
use crate::document::Document;
use crate::dto::{Conjunction, ExportScope, FilterCondition, FilterGroup, FilterNode, FilterOp};
use crate::error::{AppError, AppResult};
use crate::job::JobCtx;
use crate::schema::{self, CellState, ColumnSchema, NumericCell, SchemaIssue, TypedValue};
use crate::semantic::{self, SemanticType};

/// Default number of top text values returned when a facet does not override it.
pub const DEFAULT_TEXT_TOP_N: usize = 20;
/// Hard cap on the distinct text values one facet tracks in memory. Beyond this
/// the count map stops admitting *new* values (existing ones keep counting) and
/// the result is flagged [`FacetResult::truncated`] — the "never a full value
/// dump" memory bound for high-cardinality columns.
pub const MAX_TEXT_VALUES: usize = 10_000;
/// Default histogram bin count for numeric / date facets.
pub const DEFAULT_BINS: usize = 20;
/// Upper bound on histogram bins.
pub const MAX_BINS: usize = 100;
/// Leading rows scanned for COUNTS on an indexed (read-only) document; beyond
/// this the counts are estimated and flagged. The applied filter is unaffected.
pub const FACET_SAMPLE_ROWS: usize = 200_000;
/// Progress/cancel granularity for the streaming passes.
const CHUNK: u64 = 4096;
/// Status facets pack their positive categories into a `u64` bitmask, so at most
/// this many are tracked (extras are dropped — far more than any real report).
const MAX_STATUS_CATEGORIES: usize = 63;

// ===========================================================================
// Public DTOs
// ===========================================================================

/// The ten facet dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FacetKind {
    /// Distinct value counts (top-N + search for high-cardinality columns).
    Text,
    /// Numeric histogram + range selection.
    Number,
    /// Date range + coarse histogram.
    Date,
    /// True / false (+ blank / other) buckets.
    Boolean,
    /// Blank / null-token / invalid / value, via F31 [`schema::classify`].
    Nullability,
    /// Matches a given [`SemanticType`] or not.
    Semantic,
    /// Row-level diagnostics status (F02).
    Diagnostics,
    /// Cross-column (F27) + advisory schema (F31) validation status.
    Validation,
    /// Duplicate-group membership (F05).
    Duplicate,
    /// Bookmarked / flagged / tagged (F40).
    Annotation,
}

impl FacetKind {
    fn is_column_scoped(self) -> bool {
        matches!(
            self,
            FacetKind::Text
                | FacetKind::Number
                | FacetKind::Date
                | FacetKind::Boolean
                | FacetKind::Nullability
                | FacetKind::Semantic
        )
    }
}

/// Whether selected values keep (include) or remove (exclude) matching rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FacetMode {
    #[default]
    Include,
    Exclude,
}

/// A continuous inclusive range selection for number / date facets. Bounds are
/// carried as strings so they parse under the column's declared schema (locale,
/// date formats) exactly like a filter-builder range condition.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetRange {
    #[serde(default)]
    pub min: Option<String>,
    #[serde(default)]
    pub max: Option<String>,
}

impl FacetRange {
    fn is_empty(&self) -> bool {
        blankless(&self.min).is_none() && blankless(&self.max).is_none()
    }
}

/// One facet's active selection: OR among `values`, plus an optional continuous
/// `range` (number / date), under an include/exclude `mode`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetSelection {
    #[serde(default)]
    pub mode: FacetMode,
    /// Selected categorical value keys (text values; boolean / nullability /
    /// semantic / status bucket keys). Unused for number / date facets.
    #[serde(default)]
    pub values: Vec<String>,
    /// Continuous range (number / date facets).
    #[serde(default)]
    pub range: FacetRange,
}

impl FacetSelection {
    fn value_set(&self) -> HashSet<&str> {
        self.values.iter().map(String::as_str).collect()
    }
}

/// One facet panel's full specification: what it slices, its selection, its
/// display tuning and its (persisted, non-computational) panel layout. The
/// order of `FacetConfig::facets` is the panel order; `pinned` / `collapsed` /
/// `width` round-trip inside a saved F12 view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetSpec {
    /// Stable per-panel id (the frontend's key; echoed into results).
    pub id: String,
    pub kind: FacetKind,
    /// Stable logical column id (F12) for column-scoped facets; ignored by the
    /// four status facets.
    #[serde(default)]
    pub column_id: Option<String>,
    /// The semantic type a [`FacetKind::Semantic`] facet tests against.
    #[serde(default)]
    pub semantic: Option<SemanticType>,
    #[serde(default)]
    pub selection: FacetSelection,
    /// Text facet: how many top values to return (default [`DEFAULT_TEXT_TOP_N`]).
    #[serde(default)]
    pub top_n: Option<usize>,
    /// Text facet: case-insensitive substring narrowing the returned values.
    #[serde(default)]
    pub search: Option<String>,
    /// Number / date facet: histogram bin count (default [`DEFAULT_BINS`]).
    #[serde(default)]
    pub bins: Option<usize>,
    // ----- panel layout (persisted, never affects counts) -----
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub collapsed: bool,
    #[serde(default)]
    pub width: Option<f64>,
}

/// The saved multi-facet configuration (extends a named F12 view). Ordered by
/// panel position.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetConfig {
    #[serde(default)]
    pub facets: Vec<FacetSpec>,
}

impl FacetConfig {
    /// Whether any facet carries an active selection (used to decide whether a
    /// filter is even applied).
    pub fn any_active(&self) -> bool {
        self.facets.iter().any(|f| f.selection_is_active())
    }
}

impl FacetSpec {
    fn selection_is_active(&self) -> bool {
        !self.selection.values.is_empty() || !self.selection.range.is_empty()
    }
}

/// One bucket of a facet result: a selectable value/category with its
/// cross-filtered count.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetBucket {
    /// Stable key the frontend echoes back in [`FacetSelection::values`].
    pub key: String,
    /// Human label (equals `key` for text values).
    pub label: String,
    pub count: u64,
    /// Whether this bucket is in the facet's current selection.
    pub selected: bool,
    /// Numeric/date histogram bins carry their edges so a bin click maps to a
    /// range (timestamps for date facets).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lo: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hi: Option<f64>,
}

/// Observed extent and current selection of a number / date facet.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeInfo {
    /// Observed minimum over the scanned population (display string).
    pub min: Option<String>,
    /// Observed maximum over the scanned population (display string).
    pub max: Option<String>,
    /// The current selection bounds, echoed back verbatim.
    pub selected_min: Option<String>,
    pub selected_max: Option<String>,
}

/// One facet's computed result: its bounded buckets plus metadata.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetResult {
    pub id: String,
    pub kind: FacetKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column_id: Option<String>,
    pub mode: FacetMode,
    /// Whether this facet is currently narrowing the population.
    pub active: bool,
    /// The facet's column/inputs could not be resolved (missing column after a
    /// structural edit, or no cached data for a status facet) — it neither
    /// filters nor produces counts.
    pub unresolved: bool,
    /// Counts are estimated from a sample (large indexed document).
    pub sampled: bool,
    /// Text facet only: the value map hit [`MAX_TEXT_VALUES`]; some low-count
    /// values are not represented.
    pub truncated: bool,
    /// Text facet only: distinct values observed (may exceed returned buckets).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distinct: Option<u64>,
    /// Bounded buckets (top-N + selected + search hits for text; the fixed set
    /// otherwise).
    pub buckets: Vec<FacetBucket>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<RangeInfo>,
}

/// The full result of a facet computation over one document.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetResultSet {
    /// Document revision the counts were computed against.
    pub revision: u64,
    /// Rows in the composed (all-facets) population within the scanned range.
    pub matched_rows: usize,
    /// Total data rows in the document.
    pub total_rows: usize,
    /// Rows actually scanned (equals `total_rows` unless sampled).
    pub scanned_rows: usize,
    /// Any facet's counts are estimated from a sample.
    pub sampled: bool,
    pub facets: Vec<FacetResult>,
}

/// A facet that could not be represented as a column filter during
/// [`to_filter_group`] conversion.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DroppedFacet {
    pub id: String,
    pub reason: String,
}

/// Result of the one-way facets → filter-builder conversion.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FacetConversion {
    /// The equivalent filter tree (AND across facets). Empty group = match all.
    pub filter: FilterGroup,
    /// Facets that had no faithful column-filter equivalent and were omitted.
    pub dropped: Vec<DroppedFacet>,
}

// ===========================================================================
// Status-facet inputs (resolved by the command layer from the analysis caches)
// ===========================================================================

/// One positive category of a status facet: its key, label and the absolute
/// rows it covers (overlaps across categories are allowed — a row can carry
/// several tags or violate several rules).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusCategory {
    pub key: String,
    pub label: String,
    pub rows: Vec<usize>,
}

/// Row-level membership for one status facet dimension. Rows in no positive
/// category fall into the synthesized `none_label` bucket (e.g. "clean",
/// "unique", "none") when one is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusInput {
    pub categories: Vec<StatusCategory>,
    pub none_label: Option<String>,
}

impl StatusInput {
    /// The annotation facet, wired from a resolved [`MarkIndex`] (F40). A matched
    /// record number *is* the current absolute row, exactly as the annotation
    /// row-filter and F42 highlighting treat it.
    pub fn from_marks(index: &MarkIndex) -> StatusInput {
        let to_rows = |records: &[u64]| records.iter().map(|&r| r as usize).collect::<Vec<_>>();
        let mut categories = vec![
            StatusCategory {
                key: "starred".into(),
                label: "Bookmarked".into(),
                rows: to_rows(&index.starred),
            },
            StatusCategory {
                key: "flagged".into(),
                label: "Flagged".into(),
                rows: to_rows(&index.flagged),
            },
        ];
        for (tag, records) in &index.tagged {
            categories.push(StatusCategory {
                key: format!("tag:{tag}"),
                label: format!("Tag: {tag}"),
                rows: to_rows(records),
            });
        }
        StatusInput {
            categories,
            none_label: Some("No annotation".into()),
        }
    }

    /// The diagnostics facet, wired from a cached [`DiagnosticsReport`] (F02).
    /// One category per row-filterable issue, rows recomputed against the current
    /// document via [`diagnostics::issue_rows`].
    pub fn from_diagnostics(doc: &Document, report: &DiagnosticsReport) -> AppResult<StatusInput> {
        let mut categories = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for issue in report.source.iter().chain(report.current.iter()) {
            if !issue.row_filterable || !seen.insert(issue.id.as_str()) {
                continue;
            }
            // A stale cache entry can name a column removed by a later structural
            // edit (nothing invalidates the diagnostics cache on edit). Mirror the
            // F42 highlight engine: skip such an issue so one dead category never
            // fails the whole facet computation, degrading it to unresolved.
            if let Ok(rows) = diagnostics::issue_rows(doc, &issue.id) {
                categories.push(StatusCategory {
                    key: issue.id.clone(),
                    label: issue.title.clone(),
                    rows,
                });
            }
        }
        Ok(StatusInput {
            categories,
            none_label: Some("No issues".into()),
        })
    }

    /// The validation facet, wired from the cross-column rules (F27) and the
    /// document's advisory schema issues (F31). Two categories: any cross-rule
    /// violation, and any recorded schema-type issue.
    pub fn from_validation(
        doc: &Document,
        rules: &[CrossRule],
        schema_issues: &[SchemaIssue],
    ) -> AppResult<StatusInput> {
        let mut categories = Vec::new();
        if !rules.is_empty() {
            // Cached rules reference columns by name; a rename/delete makes
            // `violating_rows` error on resolution. Mirror F42 highlighting
            // (`.unwrap_or_default()`): a stale rule degrades the category to
            // empty rather than failing the whole computation.
            let rows = crossval::violating_rows(doc, rules, None).unwrap_or_default();
            categories.push(StatusCategory {
                key: "crossval".into(),
                label: "Cross-column violation".into(),
                rows,
            });
        }
        let n = doc.n_rows();
        let mut schema_rows: Vec<usize> = schema_issues
            .iter()
            .map(|s| s.row)
            .filter(|&r| r < n)
            .collect();
        schema_rows.sort_unstable();
        schema_rows.dedup();
        categories.push(StatusCategory {
            key: "schema".into(),
            label: "Schema type issue".into(),
            rows: schema_rows,
        });
        Ok(StatusInput {
            categories,
            none_label: Some("Valid".into()),
        })
    }

    /// The duplicate facet, wired from a dedup spec (F05): the rows in any
    /// duplicate group, against the given scope.
    pub fn from_duplicates(
        doc: &Document,
        spec: &DedupSpec,
        scope: &ExportScope,
    ) -> AppResult<StatusInput> {
        let rows = dedup::duplicate_row_indices(doc, spec, scope)?;
        Ok(StatusInput {
            categories: vec![StatusCategory {
                key: "duplicate".into(),
                label: "In a duplicate group".into(),
                rows,
            }],
            none_label: Some("Unique".into()),
        })
    }
}

/// The status-facet row memberships passed into the engine. Each field is
/// populated only when a facet of that kind is present in the config; a missing
/// field renders its facet [`FacetResult::unresolved`]. `sampled` marks that the
/// underlying reports covered only a sample.
#[derive(Debug, Clone, Default)]
pub struct FacetInputs {
    pub diagnostics: Option<StatusInput>,
    pub validation: Option<StatusInput>,
    pub duplicate: Option<StatusInput>,
    pub annotation: Option<StatusInput>,
    pub sampled: bool,
}

impl FacetInputs {
    fn for_kind(&self, kind: FacetKind) -> Option<&StatusInput> {
        match kind {
            FacetKind::Diagnostics => self.diagnostics.as_ref(),
            FacetKind::Validation => self.validation.as_ref(),
            FacetKind::Duplicate => self.duplicate.as_ref(),
            FacetKind::Annotation => self.annotation.as_ref(),
            _ => None,
        }
    }
}

// ===========================================================================
// Public entry points
// ===========================================================================

/// Compute every facet's cross-filtered bucket counts plus the composed
/// population size, in one streaming pass. Read-only; never mutates or dirties
/// the document. `ctx` (when present) reports progress and observes
/// cancellation. Counts on a large indexed document are estimated from a leading
/// sample and flagged.
pub fn compute(
    doc: &Document,
    config: &FacetConfig,
    inputs: &FacetInputs,
    ctx: Option<&JobCtx>,
) -> AppResult<FacetResultSet> {
    let scan_end = if doc.is_editable() {
        doc.n_rows()
    } else {
        FACET_SAMPLE_ROWS.min(doc.n_rows())
    };
    compute_scan(doc, config, inputs, ctx, scan_end)
}

/// Core of [`compute`] with an explicit scan bound. Rows `0..scan_end` are
/// counted; when `scan_end < n_rows` the counts are a leading-sample estimate
/// and every facet's `sampled` flag (value AND status) plus the top-level
/// [`FacetResultSet::sampled`] is set — status facets tally over the same window
/// and so are under-counted identically. Split out so the sampled path is
/// unit-testable without a multi-hundred-thousand-row indexed document.
fn compute_scan(
    doc: &Document,
    config: &FacetConfig,
    inputs: &FacetInputs,
    ctx: Option<&JobCtx>,
    scan_end: usize,
) -> AppResult<FacetResultSet> {
    let n = doc.n_rows();
    let scan_end = scan_end.min(n);
    let scan_sampled = scan_end < n;

    let mut built = build_facets(doc, config, inputs)?;
    let has_range = built.iter().any(|b| matches!(b.eval, Eval::Range(_)));

    if let Some(c) = ctx {
        let total = if has_range { scan_end * 2 } else { scan_end };
        c.set_total(total as u64);
    }

    // Pre-pass: observe numeric/date extents so histogram edges are stable.
    if has_range {
        let mut pending = 0u64;
        doc.visit_rows(0..scan_end, &mut |_, row| {
            for b in built.iter_mut() {
                if let Eval::Range(r) = &mut b.eval {
                    r.observe(row);
                }
            }
            pending += 1;
            if pending >= CHUNK {
                if let Some(c) = ctx {
                    c.advance(pending)?;
                }
                pending = 0;
            }
            Ok(true)
        })?;
        if let Some(c) = ctx {
            c.advance(pending)?;
        }
        for b in built.iter_mut() {
            if let Eval::Range(r) = &mut b.eval {
                r.finalize_bins();
            }
        }
    }

    // Counting pass with the classic all-other-facets-pass accounting.
    let mut matched = 0usize;
    let mut passes: Vec<bool> = Vec::with_capacity(built.len());
    let mut pending = 0u64;
    doc.visit_rows(0..scan_end, &mut |i, row| {
        passes.clear();
        let mut fail = 0usize;
        let mut only = 0usize;
        for (idx, b) in built.iter().enumerate() {
            let p = b.eval.passes(i, row);
            if !p {
                fail += 1;
                only = idx;
            }
            passes.push(p);
        }
        if fail == 0 {
            matched += 1;
            for b in built.iter_mut() {
                b.eval.tally(i, row);
            }
        } else if fail == 1 {
            built[only].eval.tally(i, row);
        }
        pending += 1;
        if pending >= CHUNK {
            if let Some(c) = ctx {
                c.advance(pending)?;
            }
            pending = 0;
        }
        Ok(true)
    })?;
    if let Some(c) = ctx {
        c.advance(pending)?;
    }

    let facets: Vec<FacetResult> = built
        .into_iter()
        .map(|b| b.finish(scan_sampled, inputs.sampled))
        .collect();

    Ok(FacetResultSet {
        revision: doc.revision(),
        matched_rows: matched,
        total_rows: n,
        scanned_rows: scan_end,
        sampled: scan_sampled || inputs.sampled,
        facets,
    })
}

/// Resolve the facet selection to the absolute row indices it admits, in source
/// order — the row view to hand to [`Document::set_filter`]. An **exact**
/// full-document scan (never sampled), so visible-row export respects the facet
/// filter. Inactive and unresolved facets never narrow the result.
pub fn matching_rows(
    doc: &Document,
    config: &FacetConfig,
    inputs: &FacetInputs,
) -> AppResult<Vec<usize>> {
    let built = build_facets(doc, config, inputs)?;
    let mut out = Vec::new();
    doc.visit_rows(0..doc.n_rows(), &mut |i, row| {
        if built.iter().all(|b| b.eval.passes(i, row)) {
            out.push(i);
        }
        Ok(true)
    })?;
    Ok(out)
}

/// Convert the active facets to the standard filter-builder tree (one-way,
/// lossy). Facets with no faithful column-filter equivalent — semantic and the
/// four status facets, plus some nullability/boolean exclusions — are omitted
/// and listed in [`FacetConversion::dropped`].
pub fn to_filter_group(doc: &Document, config: &FacetConfig) -> FacetConversion {
    let mut nodes = Vec::new();
    let mut dropped = Vec::new();
    for spec in &config.facets {
        if !spec.selection_is_active() {
            continue;
        }
        let col = spec
            .column_id
            .as_deref()
            .and_then(|id| resolve_col(doc, id));
        match convert_facet(spec, col) {
            Ok(Some(node)) => nodes.push(node),
            Ok(None) => {}
            Err(reason) => dropped.push(DroppedFacet {
                id: spec.id.clone(),
                reason,
            }),
        }
    }
    FacetConversion {
        filter: FilterGroup {
            conjunction: Conjunction::And,
            nodes,
        },
        dropped,
    }
}

// ===========================================================================
// Facet building
// ===========================================================================

struct BuiltFacet {
    id: String,
    kind: FacetKind,
    column_id: Option<String>,
    mode: FacetMode,
    eval: Eval,
}

enum Eval {
    Text(Box<TextEval>),
    Range(Box<RangeEval>),
    Cat(Box<CatEval>),
    Status(Box<StatusEval>),
    /// Column/inputs missing: never filters, produces no counts.
    Unresolved,
}

impl Eval {
    /// Whether the row satisfies this facet's selection. Status facets key on the
    /// absolute row index; value facets read the row slice.
    fn passes(&self, abs: usize, row: &[String]) -> bool {
        match self {
            Eval::Text(e) => e.passes(row),
            Eval::Range(e) => e.passes(row),
            Eval::Cat(e) => e.passes(row),
            Eval::Status(e) => e.passes_abs(abs),
            Eval::Unresolved => true,
        }
    }

    fn tally(&mut self, abs: usize, row: &[String]) {
        match self {
            Eval::Text(e) => e.tally(row),
            Eval::Range(e) => e.tally(row),
            Eval::Cat(e) => e.tally(row),
            Eval::Status(e) => e.tally(abs),
            Eval::Unresolved => {}
        }
    }
}

fn build_facets(
    doc: &Document,
    config: &FacetConfig,
    inputs: &FacetInputs,
) -> AppResult<Vec<BuiltFacet>> {
    let mut out = Vec::with_capacity(config.facets.len());
    for spec in &config.facets {
        let mode = spec.selection.mode;
        let eval = build_eval(doc, spec, inputs)?;
        out.push(BuiltFacet {
            id: spec.id.clone(),
            kind: spec.kind,
            column_id: spec.column_id.clone(),
            mode,
            eval,
        });
    }
    Ok(out)
}

fn build_eval(doc: &Document, spec: &FacetSpec, inputs: &FacetInputs) -> AppResult<Eval> {
    if spec.kind.is_column_scoped() {
        let Some(col) = spec
            .column_id
            .as_deref()
            .and_then(|id| resolve_col(doc, id))
        else {
            return Ok(Eval::Unresolved);
        };
        let schema = doc.column_schema_at(col).cloned();
        return build_column_eval(spec, col, schema);
    }
    // Status facet.
    match inputs.for_kind(spec.kind) {
        Some(input) => Ok(Eval::Status(Box::new(StatusEval::build(spec, input)))),
        None => Ok(Eval::Unresolved),
    }
}

fn build_column_eval(
    spec: &FacetSpec,
    col: usize,
    schema: Option<ColumnSchema>,
) -> AppResult<Eval> {
    let sel = &spec.selection;
    match spec.kind {
        FacetKind::Text => Ok(Eval::Text(Box::new(TextEval::build(spec, col)))),
        FacetKind::Number => Ok(Eval::Range(Box::new(RangeEval::build(
            spec, col, schema, false,
        )?))),
        FacetKind::Date => Ok(Eval::Range(Box::new(RangeEval::build(
            spec, col, schema, true,
        )?))),
        FacetKind::Boolean => Ok(Eval::Cat(Box::new(CatEval::boolean(sel, col, schema)))),
        FacetKind::Nullability => Ok(Eval::Cat(Box::new(CatEval::nullability(sel, col, schema)))),
        FacetKind::Semantic => {
            let Some(sem) = spec.semantic else {
                return Ok(Eval::Unresolved);
            };
            Ok(Eval::Cat(Box::new(CatEval::semantic(sel, col, sem))))
        }
        _ => Ok(Eval::Unresolved),
    }
}

impl BuiltFacet {
    fn finish(self, scan_sampled: bool, inputs_sampled: bool) -> FacetResult {
        // Every facet's counts are tallied over the same `0..scan_end` window, so
        // a truncated scan under-counts the four status facets exactly as it does
        // value facets — flag both on `scan_sampled`. `inputs_sampled`
        // additionally marks a status facet whose underlying report was itself
        // only a sample.
        let sampled = scan_sampled || inputs_sampled;
        let base = FacetResult {
            id: self.id,
            kind: self.kind,
            column_id: self.column_id,
            mode: self.mode,
            active: false,
            unresolved: false,
            sampled,
            truncated: false,
            distinct: None,
            buckets: Vec::new(),
            range: None,
        };
        match self.eval {
            Eval::Text(e) => e.finish(base),
            Eval::Range(e) => e.finish(base),
            Eval::Cat(e) => e.finish(base),
            Eval::Status(e) => e.finish(base),
            Eval::Unresolved => FacetResult {
                unresolved: true,
                ..base
            },
        }
    }
}

// ===========================================================================
// Text facet
// ===========================================================================

struct TextEval {
    col: usize,
    mode: FacetMode,
    selected: HashSet<String>,
    active: bool,
    top_n: usize,
    search: Option<String>,
    counts: HashMap<String, u64>,
    truncated: bool,
}

impl TextEval {
    fn build(spec: &FacetSpec, col: usize) -> TextEval {
        let selected: HashSet<String> = spec.selection.values.iter().cloned().collect();
        TextEval {
            col,
            mode: spec.selection.mode,
            active: !selected.is_empty(),
            selected,
            top_n: spec.top_n.unwrap_or(DEFAULT_TEXT_TOP_N),
            search: blankless(&spec.search).map(|s| s.to_lowercase()),
            counts: HashMap::new(),
            truncated: false,
        }
    }

    fn passes(&self, row: &[String]) -> bool {
        if !self.active {
            return true;
        }
        let matched = self.selected.contains(cell(row, self.col));
        match self.mode {
            FacetMode::Include => matched,
            FacetMode::Exclude => !matched,
        }
    }

    fn tally(&mut self, row: &[String]) {
        let v = cell(row, self.col);
        if let Some(c) = self.counts.get_mut(v) {
            *c += 1;
        } else if self.counts.len() < MAX_TEXT_VALUES {
            self.counts.insert(v.to_string(), 1);
        } else {
            self.truncated = true;
        }
    }

    fn finish(self, base: FacetResult) -> FacetResult {
        let distinct = self.counts.len() as u64;
        let mut buckets: Vec<FacetBucket> = Vec::new();
        let mut placed: HashSet<&str> = HashSet::new();

        // Selected values are always shown (checked), even at count 0 — but bound
        // the emitted buckets by MAX_TEXT_VALUES so a pathological or hand-edited
        // saved view with a huge `selection.values` array can never produce an
        // unbounded payload (the "never a full dump" guarantee). Every selected
        // key is still marked `placed` (so it is excluded from `rest`) and still
        // filters exactly via `self.selected` in `passes`; only the DISPLAY list
        // is capped. The cap is deterministic (sorted, leading MAX_TEXT_VALUES).
        let mut sorted_selected: Vec<&String> = self.selected.iter().collect();
        sorted_selected.sort();
        for (i, key) in sorted_selected.iter().enumerate() {
            placed.insert(key.as_str());
            if i < MAX_TEXT_VALUES {
                let count = self.counts.get(*key).copied().unwrap_or(0);
                buckets.push(text_bucket(key, count, true));
            }
        }

        // Then the top values by count among the (search-narrowed) remainder.
        let mut rest: Vec<(&String, u64)> = self
            .counts
            .iter()
            .filter(|(k, _)| !placed.contains(k.as_str()))
            .filter(|(k, _)| match &self.search {
                Some(s) => k.to_lowercase().contains(s.as_str()),
                None => true,
            })
            .map(|(k, v)| (k, *v))
            .collect();
        rest.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        for (key, count) in rest.into_iter().take(self.top_n) {
            buckets.push(text_bucket(key, count, false));
        }

        FacetResult {
            active: self.active,
            truncated: self.truncated,
            distinct: Some(distinct),
            buckets,
            ..base
        }
    }
}

fn text_bucket(key: &str, count: u64, selected: bool) -> FacetBucket {
    FacetBucket {
        key: key.to_string(),
        label: key.to_string(),
        count,
        selected,
        lo: None,
        hi: None,
    }
}

// ===========================================================================
// Number / date facet (histogram + continuous range)
// ===========================================================================

struct RangeEval {
    col: usize,
    date: bool,
    schema: Option<ColumnSchema>,
    mode: FacetMode,
    active: bool,
    min: Option<f64>,
    max: Option<f64>,
    sel_min: Option<String>,
    sel_max: Option<String>,
    nbins: usize,
    obs_min: Option<f64>,
    obs_max: Option<f64>,
    edges: Vec<f64>,
    counts: Vec<u64>,
}

impl RangeEval {
    fn build(
        spec: &FacetSpec,
        col: usize,
        schema: Option<ColumnSchema>,
        date: bool,
    ) -> AppResult<RangeEval> {
        let range = &spec.selection.range;
        let sel_min = blankless(&range.min).map(str::to_string);
        let sel_max = blankless(&range.max).map(str::to_string);
        let parse = |s: &Option<String>| -> AppResult<Option<f64>> {
            match s {
                Some(v) => Ok(Some(parse_bound(v, schema.as_ref(), date)?)),
                None => Ok(None),
            }
        };
        let min = parse(&sel_min)?;
        let max = parse(&sel_max)?;
        Ok(RangeEval {
            col,
            date,
            schema,
            mode: spec.selection.mode,
            active: min.is_some() || max.is_some(),
            min,
            max,
            sel_min,
            sel_max,
            nbins: spec.bins.unwrap_or(DEFAULT_BINS).clamp(1, MAX_BINS),
            obs_min: None,
            obs_max: None,
            edges: Vec::new(),
            counts: Vec::new(),
        })
    }

    fn value(&self, s: &str) -> Option<f64> {
        if self.date {
            date_value(s, self.schema.as_ref())
        } else {
            numeric_value(s, self.schema.as_ref())
        }
    }

    fn in_range(&self, v: f64) -> bool {
        self.min.is_none_or(|lo| v >= lo) && self.max.is_none_or(|hi| v <= hi)
    }

    fn passes(&self, row: &[String]) -> bool {
        if !self.active {
            return true;
        }
        let matched = self
            .value(cell(row, self.col))
            .is_some_and(|v| self.in_range(v));
        match self.mode {
            FacetMode::Include => matched,
            FacetMode::Exclude => !matched,
        }
    }

    fn observe(&mut self, row: &[String]) {
        if let Some(v) = self.value(cell(row, self.col)) {
            self.obs_min = Some(self.obs_min.map_or(v, |m| m.min(v)));
            self.obs_max = Some(self.obs_max.map_or(v, |m| m.max(v)));
        }
    }

    fn finalize_bins(&mut self) {
        let (Some(lo), Some(hi)) = (self.obs_min, self.obs_max) else {
            return;
        };
        if lo >= hi {
            self.edges = vec![lo, hi];
            self.counts = vec![0];
            return;
        }
        let n = self.nbins.max(1);
        let step = (hi - lo) / n as f64;
        let mut edges: Vec<f64> = (0..=n).map(|i| lo + step * i as f64).collect();
        *edges.last_mut().unwrap() = hi; // pin the top edge exactly
        self.counts = vec![0; n];
        self.edges = edges;
    }

    fn bin_of(&self, v: f64) -> usize {
        let n = self.counts.len();
        if n == 0 {
            return 0;
        }
        if v <= self.edges[0] {
            return 0;
        }
        if v >= self.edges[n] {
            return n - 1;
        }
        // Linear scan (n <= MAX_BINS): the bin whose upper edge first exceeds v.
        for (i, w) in self.edges.windows(2).enumerate() {
            if v < w[1] {
                return i;
            }
        }
        n - 1
    }

    fn tally(&mut self, row: &[String]) {
        if self.counts.is_empty() {
            return;
        }
        if let Some(v) = self.value(cell(row, self.col)) {
            let bin = self.bin_of(v);
            self.counts[bin] += 1;
        }
    }

    /// Format a numeric edge or a date-timestamp edge for display.
    fn fmt_edge(&self, v: f64) -> String {
        if self.date {
            fmt_ts(v as i64, is_datetime(self.schema.as_ref()))
        } else {
            fmt_num(v)
        }
    }

    fn finish(self, base: FacetResult) -> FacetResult {
        let mut buckets = Vec::with_capacity(self.counts.len());
        for (i, &count) in self.counts.iter().enumerate() {
            let lo = self.edges[i];
            let hi = self.edges[i + 1];
            buckets.push(FacetBucket {
                key: format!("b{i}"),
                label: format!("{} – {}", self.fmt_edge(lo), self.fmt_edge(hi)),
                count,
                selected: false,
                lo: Some(lo),
                hi: Some(hi),
            });
        }
        let range = RangeInfo {
            min: self.obs_min.map(|v| self.fmt_edge(v)),
            max: self.obs_max.map(|v| self.fmt_edge(v)),
            selected_min: self.sel_min,
            selected_max: self.sel_max,
        };
        FacetResult {
            active: self.active,
            buckets,
            range: Some(range),
            ..base
        }
    }
}

// ===========================================================================
// Categorical facet (boolean / nullability / semantic — one category per row)
// ===========================================================================

struct CatEval {
    col: usize,
    classifier: CatClassifier,
    categories: Vec<CatDef>,
    selected: u64,
    active: bool,
    mode: FacetMode,
    counts: Vec<u64>,
}

struct CatDef {
    key: &'static str,
    label: String,
}

enum CatClassifier {
    Boolean(Option<ColumnSchema>),
    Nullability(ColumnSchema),
    Semantic(SemanticType),
}

impl CatEval {
    fn new(
        col: usize,
        classifier: CatClassifier,
        categories: Vec<CatDef>,
        sel: &FacetSelection,
    ) -> CatEval {
        let chosen = sel.value_set();
        let mut selected = 0u64;
        for (i, c) in categories.iter().enumerate() {
            if chosen.contains(c.key) {
                selected |= 1 << i;
            }
        }
        let counts = vec![0; categories.len()];
        CatEval {
            col,
            classifier,
            categories,
            active: selected != 0,
            selected,
            mode: sel.mode,
            counts,
        }
    }

    fn boolean(sel: &FacetSelection, col: usize, schema: Option<ColumnSchema>) -> CatEval {
        let cats = vec![
            CatDef {
                key: "true",
                label: "True".into(),
            },
            CatDef {
                key: "false",
                label: "False".into(),
            },
            CatDef {
                key: "blank",
                label: "Blank".into(),
            },
            CatDef {
                key: "other",
                label: "Other".into(),
            },
        ];
        CatEval::new(col, CatClassifier::Boolean(schema), cats, sel)
    }

    fn nullability(sel: &FacetSelection, col: usize, schema: Option<ColumnSchema>) -> CatEval {
        // Without a declared schema, classify against permissive text so blanks
        // still separate from values (null-token / invalid never arise).
        let schema =
            schema.unwrap_or_else(|| ColumnSchema::new("", "", crate::schema::LogicalType::Text));
        let cats = vec![
            CatDef {
                key: "value",
                label: "Has value".into(),
            },
            CatDef {
                key: "blank",
                label: "Blank".into(),
            },
            CatDef {
                key: "null",
                label: "Null token".into(),
            },
            CatDef {
                key: "invalid",
                label: "Invalid".into(),
            },
        ];
        CatEval::new(col, CatClassifier::Nullability(schema), cats, sel)
    }

    fn semantic(sel: &FacetSelection, col: usize, sem: SemanticType) -> CatEval {
        let cats = vec![
            CatDef {
                key: "match",
                label: "Matches".into(),
            },
            CatDef {
                key: "mismatch",
                label: "Doesn't match".into(),
            },
            CatDef {
                key: "blank",
                label: "Blank".into(),
            },
        ];
        CatEval::new(col, CatClassifier::Semantic(sem), cats, sel)
    }

    fn classify(&self, s: &str) -> usize {
        match &self.classifier {
            CatClassifier::Boolean(schema) => bool_bucket(s, schema.as_ref()),
            CatClassifier::Nullability(schema) => match schema::classify(Some(s), schema) {
                CellState::Valid(_) => 0,
                CellState::Empty | CellState::Missing => 1,
                CellState::NullToken => 2,
                CellState::Invalid(_) => 3,
            },
            CatClassifier::Semantic(sem) => {
                if s.trim().is_empty() {
                    2
                } else if semantic::matches_type(s, *sem) {
                    0
                } else {
                    1
                }
            }
        }
    }

    fn passes(&self, row: &[String]) -> bool {
        if !self.active {
            return true;
        }
        let cat = self.classify(cell(row, self.col));
        let matched = (self.selected >> cat) & 1 == 1;
        match self.mode {
            FacetMode::Include => matched,
            FacetMode::Exclude => !matched,
        }
    }

    fn tally(&mut self, row: &[String]) {
        let cat = self.classify(cell(row, self.col));
        self.counts[cat] += 1;
    }

    fn finish(self, base: FacetResult) -> FacetResult {
        let buckets = self
            .categories
            .iter()
            .enumerate()
            .map(|(i, c)| FacetBucket {
                key: c.key.to_string(),
                label: c.label.clone(),
                count: self.counts[i],
                selected: (self.selected >> i) & 1 == 1,
                lo: None,
                hi: None,
            })
            .collect();
        FacetResult {
            active: self.active,
            buckets,
            ..base
        }
    }
}

// ===========================================================================
// Status facet (diagnostics / validation / duplicate / annotation)
// ===========================================================================

struct StatusEval {
    row_mask: HashMap<usize, u64>,
    categories: Vec<StatusCategory>,
    none_label: Option<String>,
    selected: u64,
    none_selected: bool,
    active: bool,
    mode: FacetMode,
    counts: Vec<u64>,
    none_count: u64,
}

impl StatusEval {
    fn build(spec: &FacetSpec, input: &StatusInput) -> StatusEval {
        let categories: Vec<StatusCategory> = input
            .categories
            .iter()
            .take(MAX_STATUS_CATEGORIES)
            .cloned()
            .collect();
        let mut row_mask: HashMap<usize, u64> = HashMap::new();
        for (i, cat) in categories.iter().enumerate() {
            let bit = 1u64 << i;
            for &row in &cat.rows {
                *row_mask.entry(row).or_insert(0) |= bit;
            }
        }
        let chosen = spec.selection.value_set();
        let mut selected = 0u64;
        for (i, cat) in categories.iter().enumerate() {
            if chosen.contains(cat.key.as_str()) {
                selected |= 1 << i;
            }
        }
        let none_selected = input.none_label.is_some() && chosen.contains(NONE_KEY);
        StatusEval {
            row_mask,
            counts: vec![0; categories.len()],
            categories,
            none_label: input.none_label.clone(),
            active: selected != 0 || none_selected,
            selected,
            none_selected,
            mode: spec.selection.mode,
            none_count: 0,
        }
    }

    fn mask(&self, abs: usize) -> u64 {
        self.row_mask.get(&abs).copied().unwrap_or(0)
    }

    fn passes_abs(&self, abs: usize) -> bool {
        if !self.active {
            return true;
        }
        let mask = self.mask(abs);
        let matched = (mask & self.selected != 0) || (mask == 0 && self.none_selected);
        match self.mode {
            FacetMode::Include => matched,
            FacetMode::Exclude => !matched,
        }
    }

    fn tally(&mut self, abs: usize) {
        let mask = self.mask(abs);
        if mask == 0 {
            self.none_count += 1;
            return;
        }
        for (i, c) in self.counts.iter_mut().enumerate() {
            if (mask >> i) & 1 == 1 {
                *c += 1;
            }
        }
    }

    fn finish(self, base: FacetResult) -> FacetResult {
        let mut buckets: Vec<FacetBucket> = self
            .categories
            .iter()
            .enumerate()
            .map(|(i, c)| FacetBucket {
                key: c.key.clone(),
                label: c.label.clone(),
                count: self.counts[i],
                selected: (self.selected >> i) & 1 == 1,
                lo: None,
                hi: None,
            })
            .collect();
        if let Some(label) = &self.none_label {
            buckets.push(FacetBucket {
                key: NONE_KEY.to_string(),
                label: label.clone(),
                count: self.none_count,
                selected: self.none_selected,
                lo: None,
                hi: None,
            });
        }
        FacetResult {
            active: self.active,
            buckets,
            ..base
        }
    }
}

/// Reserved selection key for a status facet's synthesized "none" bucket.
const NONE_KEY: &str = "__none__";

// ===========================================================================
// facets -> filter-builder conversion
// ===========================================================================

fn convert_facet(spec: &FacetSpec, col: Option<usize>) -> Result<Option<FilterNode>, String> {
    let sel = &spec.selection;
    let include = sel.mode == FacetMode::Include;
    match spec.kind {
        FacetKind::Text => {
            let col = col.ok_or_else(|| "column not found".to_string())?;
            let (op, conj) = if include {
                (FilterOp::Equals, Conjunction::Or)
            } else {
                (FilterOp::NotEquals, Conjunction::And)
            };
            let nodes = sel
                .values
                .iter()
                .map(|v| condition(col, op, v, true))
                .collect();
            Ok(Some(FilterNode::Group(FilterGroup {
                conjunction: conj,
                nodes,
            })))
        }
        FacetKind::Number | FacetKind::Date => {
            let col = col.ok_or_else(|| "column not found".to_string())?;
            let min = blankless(&sel.range.min);
            let max = blankless(&sel.range.max);
            if include {
                let mut nodes = Vec::new();
                if let Some(lo) = min {
                    nodes.push(condition(col, FilterOp::Gte, lo, false));
                }
                if let Some(hi) = max {
                    nodes.push(condition(col, FilterOp::Lte, hi, false));
                }
                Ok(Some(FilterNode::Group(FilterGroup {
                    conjunction: Conjunction::And,
                    nodes,
                })))
            } else {
                // Complement of a range: below min OR above max.
                let mut nodes = Vec::new();
                if let Some(lo) = min {
                    nodes.push(condition(col, FilterOp::Lt, lo, false));
                }
                if let Some(hi) = max {
                    nodes.push(condition(col, FilterOp::Gt, hi, false));
                }
                Ok(Some(FilterNode::Group(FilterGroup {
                    conjunction: Conjunction::Or,
                    nodes,
                })))
            }
        }
        FacetKind::Nullability => {
            if !include {
                return Err("exclude-mode nullability has no filter equivalent".into());
            }
            let col = col.ok_or_else(|| "column not found".to_string())?;
            let chosen = sel.value_set();
            // Only the cleanly expressible single-bucket selections convert.
            if chosen.len() == 1 && chosen.contains("blank") {
                Ok(Some(FilterNode::Condition(FilterCondition {
                    column: col,
                    op: FilterOp::IsEmpty,
                    value: String::new(),
                    case_sensitive: false,
                })))
            } else if chosen.len() == 1 && chosen.contains("value") {
                Ok(Some(FilterNode::Condition(FilterCondition {
                    column: col,
                    op: FilterOp::NotEmpty,
                    value: String::new(),
                    case_sensitive: false,
                })))
            } else {
                Err("this nullability selection has no exact filter equivalent".into())
            }
        }
        FacetKind::Boolean => {
            Err("boolean facets don't convert to a filter (multiple spellings)".into())
        }
        FacetKind::Semantic => Err("semantic facets have no column-filter equivalent".into()),
        FacetKind::Diagnostics
        | FacetKind::Validation
        | FacetKind::Duplicate
        | FacetKind::Annotation => Err("status facets have no column-filter equivalent".into()),
    }
}

fn condition(column: usize, op: FilterOp, value: &str, case_sensitive: bool) -> FilterNode {
    FilterNode::Condition(FilterCondition {
        column,
        op,
        value: value.to_string(),
        case_sensitive,
    })
}

// ===========================================================================
// Shared value helpers
// ===========================================================================

fn cell(row: &[String], col: usize) -> &str {
    row.get(col).map(String::as_str).unwrap_or("")
}

fn resolve_col(doc: &Document, column_id: &str) -> Option<usize> {
    doc.column_ids().iter().position(|id| id == column_id)
}

/// `Some(trimmed)` unless the option is absent or blank.
fn blankless(s: &Option<String>) -> Option<&str> {
    match s {
        Some(v) if !v.trim().is_empty() => Some(v.trim()),
        _ => None,
    }
}

fn numeric_value(s: &str, schema: Option<&ColumnSchema>) -> Option<f64> {
    if let Some(sc) = schema {
        if sc.logical_type.is_numeric() {
            return match schema::numeric_cell(sc, s) {
                NumericCell::Value(v) => Some(v),
                _ => None,
            };
        }
    }
    analyze::as_number(s)
}

fn date_value(s: &str, schema: Option<&ColumnSchema>) -> Option<f64> {
    if s.trim().is_empty() {
        return None;
    }
    if let Some(sc) = schema {
        if sc.logical_type.is_temporal() {
            return match schema::classify(Some(s), sc) {
                CellState::Valid(TypedValue::Date(d)) => {
                    Some(d.and_hms_opt(0, 0, 0)?.and_utc().timestamp() as f64)
                }
                CellState::Valid(TypedValue::DateTime(dt)) => Some(dt.and_utc().timestamp() as f64),
                _ => None,
            };
        }
    }
    analyze::parse_date(s).map(|dt| dt.and_utc().timestamp() as f64)
}

fn parse_bound(s: &str, schema: Option<&ColumnSchema>, date: bool) -> AppResult<f64> {
    let parsed = if date {
        date_value(s, schema)
    } else {
        numeric_value(s, schema)
    };
    parsed.ok_or_else(|| {
        AppError::invalid(format!(
            "'{s}' is not a valid {} bound",
            if date { "date" } else { "numeric" }
        ))
    })
}

fn is_datetime(schema: Option<&ColumnSchema>) -> bool {
    matches!(
        schema.map(|s| s.logical_type),
        Some(crate::schema::LogicalType::Datetime)
    )
}

/// Boolean bucket index: 0 true, 1 false, 2 blank, 3 other. Uses the declared
/// schema when present, else a permissive heuristic (numeric flags count).
fn bool_bucket(s: &str, schema: Option<&ColumnSchema>) -> usize {
    if let Some(sc) = schema {
        if sc.logical_type == crate::schema::LogicalType::Boolean {
            return match schema::classify(Some(s), sc) {
                CellState::Valid(TypedValue::Boolean(true)) => 0,
                CellState::Valid(TypedValue::Boolean(false)) => 1,
                CellState::Empty | CellState::Missing | CellState::NullToken => 2,
                _ => 3,
            };
        }
    }
    let t = s.trim();
    if t.is_empty() {
        return 2;
    }
    match t.to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" | "t" | "y" => 0,
        "false" | "no" | "0" | "f" | "n" => 1,
        _ => 3,
    }
}

fn fmt_num(v: f64) -> String {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 1e15 {
        (v as i64).to_string()
    } else {
        let s = format!("{v:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

fn fmt_ts(secs: i64, datetime: bool) -> String {
    match chrono::DateTime::from_timestamp(secs, 0) {
        Some(dt) => {
            let naive = dt.naive_utc();
            if datetime {
                naive.format("%Y-%m-%d %H:%M:%S").to_string()
            } else {
                naive.format("%Y-%m-%d").to_string()
            }
        }
        None => secs.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotations::{AnnotationStore, RowMarkPatch};
    use crate::parse::{parse, ParseSettings};
    use crate::schema::LogicalType;
    use crate::tabular::DocumentSource;

    fn doc(csv: &str) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, true)
    }

    fn spec(id: &str, kind: FacetKind, column_id: &str) -> FacetSpec {
        FacetSpec {
            id: id.into(),
            kind,
            column_id: Some(column_id.into()),
            semantic: None,
            selection: FacetSelection::default(),
            top_n: None,
            search: None,
            bins: None,
            pinned: false,
            collapsed: false,
            width: None,
        }
    }

    fn include(values: &[&str]) -> FacetSelection {
        FacetSelection {
            mode: FacetMode::Include,
            values: values.iter().map(|s| s.to_string()).collect(),
            range: FacetRange::default(),
        }
    }

    fn find<'a>(rs: &'a FacetResultSet, id: &str) -> &'a FacetResult {
        rs.facets
            .iter()
            .find(|f| f.id == id)
            .expect("facet present")
    }

    fn count(f: &FacetResult, key: &str) -> u64 {
        f.buckets
            .iter()
            .find(|b| b.key == key)
            .map(|b| b.count)
            .unwrap_or_else(|| panic!("bucket {key} present"))
    }

    // city,age,active
    // 0 NYC,20,true
    // 1 NYC,30,false
    // 2 LA,20,true
    // 3 LA,40,true
    // 4 NYC,30,true
    fn sample_doc() -> Document {
        doc("city,age,active\nNYC,20,true\nNYC,30,false\nLA,20,true\nLA,40,true\nNYC,30,true\n")
    }

    fn config_city_age_bool() -> FacetConfig {
        let mut age = spec("age", FacetKind::Number, "c1");
        age.bins = Some(2); // edges [20,30,40]: b0=[20,30) b1=[30,40]
        FacetConfig {
            facets: vec![
                spec("city", FacetKind::Text, "c0"),
                age,
                spec("active", FacetKind::Boolean, "c2"),
            ],
        }
    }

    #[test]
    fn unfiltered_counts_are_full_population() {
        let d = sample_doc();
        let cfg = config_city_age_bool();
        let rs = compute(&d, &cfg, &FacetInputs::default(), None).unwrap();
        assert_eq!(rs.matched_rows, 5);
        assert_eq!(rs.total_rows, 5);
        assert!(!rs.sampled);

        let city = find(&rs, "city");
        assert_eq!(count(city, "NYC"), 3);
        assert_eq!(count(city, "LA"), 2);
        assert_eq!(city.distinct, Some(2));

        // age b0=[20,30) -> rows 0,2 ; b1=[30,40] -> rows 1,3,4
        let age = find(&rs, "age");
        assert_eq!(count(age, "b0"), 2);
        assert_eq!(count(age, "b1"), 3);

        let active = find(&rs, "active");
        assert_eq!(count(active, "true"), 4);
        assert_eq!(count(active, "false"), 1);
    }

    #[test]
    fn cross_filter_counts_reflect_other_facets() {
        let d = sample_doc();
        let mut cfg = config_city_age_bool();
        cfg.facets[0].selection = include(&["NYC"]); // city = NYC {0,1,4}

        let rs = compute(&d, &cfg, &FacetInputs::default(), None).unwrap();
        // Only city active -> population is NYC = 3.
        assert_eq!(rs.matched_rows, 3);

        // City counts reflect all OTHER facets (none active) -> full.
        let city = find(&rs, "city");
        assert_eq!(count(city, "NYC"), 3);
        assert_eq!(count(city, "LA"), 2);
        assert!(
            city.buckets
                .iter()
                .find(|b| b.key == "NYC")
                .unwrap()
                .selected
        );

        // Boolean counts over the city=NYC population {0,1,4}: true=2,false=1.
        let active = find(&rs, "active");
        assert_eq!(count(active, "true"), 2);
        assert_eq!(count(active, "false"), 1);

        // Age counts over {0,1,4}: ages 20,30,30 -> b0=1, b1=2.
        let age = find(&rs, "age");
        assert_eq!(count(age, "b0"), 1);
        assert_eq!(count(age, "b1"), 2);
    }

    #[test]
    fn two_active_facets_and_clear_retention() {
        let d = sample_doc();
        let mut cfg = config_city_age_bool();
        cfg.facets[0].selection = include(&["NYC"]); // city=NYC {0,1,4}
        cfg.facets[2].selection = include(&["true"]); // active=true {0,2,3,4}

        let rs = compute(&d, &cfg, &FacetInputs::default(), None).unwrap();
        // city=NYC AND active=true -> {0,4}
        assert_eq!(rs.matched_rows, 2);

        // City counts against the OTHER facets (active=true) population {0,2,3,4}:
        // NYC among those = {0,4}=2, LA = {2,3}=2.
        let city = find(&rs, "city");
        assert_eq!(count(city, "NYC"), 2);
        assert_eq!(count(city, "LA"), 2);

        // Boolean counts against city=NYC {0,1,4}: true=2, false=1.
        let active = find(&rs, "active");
        assert_eq!(count(active, "true"), 2);
        assert_eq!(count(active, "false"), 1);

        // Age counts against city=NYC AND active=true = {0,4}: ages 20,30 -> b0=1,b1=1.
        let age = find(&rs, "age");
        assert_eq!(count(age, "b0"), 1);
        assert_eq!(count(age, "b1"), 1);

        // Clearing the boolean facet must leave city's selection intact and its
        // counts revert to the full population.
        cfg.facets[2].selection = FacetSelection::default();
        let rs2 = compute(&d, &cfg, &FacetInputs::default(), None).unwrap();
        assert_eq!(rs2.matched_rows, 3); // just city=NYC again
        let city2 = find(&rs2, "city");
        assert_eq!(count(city2, "NYC"), 3);
        assert_eq!(count(city2, "LA"), 2);
    }

    #[test]
    fn include_exclude_is_deterministic() {
        let d = sample_doc();
        let inputs = FacetInputs::default();

        let mut inc = FacetConfig {
            facets: vec![spec("city", FacetKind::Text, "c0")],
        };
        inc.facets[0].selection = include(&["NYC"]);
        assert_eq!(matching_rows(&d, &inc, &inputs).unwrap(), vec![0, 1, 4]);

        let mut exc = inc.clone();
        exc.facets[0].selection.mode = FacetMode::Exclude;
        assert_eq!(matching_rows(&d, &exc, &inputs).unwrap(), vec![2, 3]);
    }

    #[test]
    fn numeric_range_selection_applies_exactly() {
        let d = sample_doc();
        let inputs = FacetInputs::default();
        let mut cfg = FacetConfig {
            facets: vec![spec("age", FacetKind::Number, "c1")],
        };
        cfg.facets[0].selection = FacetSelection {
            mode: FacetMode::Include,
            values: vec![],
            range: FacetRange {
                min: Some("25".into()),
                max: None,
            },
        };
        // age >= 25 -> rows 1(30),3(40),4(30)
        assert_eq!(matching_rows(&d, &cfg, &inputs).unwrap(), vec![1, 3, 4]);
    }

    #[test]
    fn matching_rows_matches_all_when_inactive() {
        let d = sample_doc();
        let cfg = config_city_age_bool();
        assert_eq!(
            matching_rows(&d, &cfg, &FacetInputs::default()).unwrap(),
            vec![0, 1, 2, 3, 4]
        );
    }

    #[test]
    fn compute_and_apply_are_non_destructive() {
        let mut d = sample_doc();
        let rev = d.revision();
        let mut cfg = config_city_age_bool();
        cfg.facets[0].selection = include(&["NYC"]);

        // Computing counts touches nothing.
        let _ = compute(&d, &cfg, &FacetInputs::default(), None).unwrap();
        assert_eq!(d.revision(), rev);
        assert!(!d.is_dirty());

        // Applying facets sets the row view like a filter — never dirties.
        let rows = matching_rows(&d, &cfg, &FacetInputs::default()).unwrap();
        d.set_filter(rows).unwrap();
        assert!(!d.is_dirty());
        assert_eq!(d.visible_len(), 3);
    }

    #[test]
    fn text_facet_is_bounded_and_truncates() {
        // 12_000 distinct values trips the MAX_TEXT_VALUES cap.
        let mut csv = String::from("id\n");
        for i in 0..12_000 {
            csv.push_str(&format!("v{i}\n"));
        }
        let d = doc(&csv);
        let mut s = spec("ids", FacetKind::Text, "c0");
        s.top_n = Some(5);
        let cfg = FacetConfig { facets: vec![s] };
        let rs = compute(&d, &cfg, &FacetInputs::default(), None).unwrap();
        let f = find(&rs, "ids");
        assert!(f.truncated, "distinct beyond the cap must flag truncated");
        assert_eq!(f.distinct, Some(MAX_TEXT_VALUES as u64));
        assert!(
            f.buckets.len() <= 5,
            "bounded DTO: at most top_n buckets, got {}",
            f.buckets.len()
        );
    }

    #[test]
    fn text_search_narrows_returned_values() {
        let d = doc("name\napple\napricot\nbanana\ncherry\n");
        let mut s = spec("name", FacetKind::Text, "c0");
        s.search = Some("ap".into());
        let cfg = FacetConfig { facets: vec![s] };
        let rs = compute(&d, &cfg, &FacetInputs::default(), None).unwrap();
        let f = find(&rs, "name");
        let keys: HashSet<&str> = f.buckets.iter().map(|b| b.key.as_str()).collect();
        assert_eq!(keys, HashSet::from(["apple", "apricot"]));
        assert_eq!(f.distinct, Some(4)); // distinct still counts the whole column
    }

    #[test]
    fn nullability_distinguishes_blank_null_invalid_value() {
        // Declared integer column with a NULL token; row 2 blank, row 3 invalid.
        let mut d = doc("n,k\n10,a\nNULL,b\n,c\nxx,d\n");
        let mut schema = ColumnSchema::new(d.column_ids()[0].clone(), "n", LogicalType::Integer);
        schema.null_tokens = vec!["NULL".into()];
        d.set_column_schema(schema);

        let cfg = FacetConfig {
            facets: vec![spec("null", FacetKind::Nullability, "c0")],
        };
        let rs = compute(&d, &cfg, &FacetInputs::default(), None).unwrap();
        let f = find(&rs, "null");
        assert_eq!(count(f, "value"), 1); // "10"
        assert_eq!(count(f, "null"), 1); // "NULL"
        assert_eq!(count(f, "blank"), 1); // ""
        assert_eq!(count(f, "invalid"), 1); // "xx"
    }

    #[test]
    fn view_config_round_trips_through_json() {
        let mut age = spec("age", FacetKind::Number, "c1");
        age.bins = Some(8);
        age.selection = FacetSelection {
            mode: FacetMode::Include,
            values: vec![],
            range: FacetRange {
                min: Some("18".into()),
                max: Some("65".into()),
            },
        };
        age.pinned = true;
        age.width = Some(240.0);
        let mut city = spec("city", FacetKind::Text, "c0");
        city.selection = include(&["NYC", "LA"]);
        city.selection.mode = FacetMode::Exclude;
        city.top_n = Some(30);
        let cfg = FacetConfig {
            facets: vec![city, age],
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: FacetConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn conversion_matches_the_facet_filter_for_convertible_facets() {
        let d = sample_doc();
        let inputs = FacetInputs::default();

        // Text include + numeric range: both should convert to a filter that
        // selects exactly the same rows the facet engine would.
        let mut cfg = FacetConfig {
            facets: vec![
                spec("city", FacetKind::Text, "c0"),
                spec("age", FacetKind::Number, "c1"),
            ],
        };
        cfg.facets[0].selection = include(&["NYC"]);
        cfg.facets[1].selection = FacetSelection {
            mode: FacetMode::Include,
            values: vec![],
            range: FacetRange {
                min: Some("25".into()),
                max: None,
            },
        };
        let conv = to_filter_group(&d, &cfg);
        assert!(conv.dropped.is_empty());
        let via_filter = crate::filter::matching_rows(&d, &conv.filter).unwrap();
        let via_facets = matching_rows(&d, &cfg, &inputs).unwrap();
        assert_eq!(via_filter, via_facets);
        // city=NYC AND age>=25 -> rows 1,4
        assert_eq!(via_facets, vec![1, 4]);
    }

    #[test]
    fn conversion_reports_dropped_status_and_semantic_facets() {
        let d = sample_doc();
        let mut sem = spec("sem", FacetKind::Semantic, "c0");
        sem.semantic = Some(SemanticType::Email);
        sem.selection = include(&["match"]);
        let mut ann = FacetSpec {
            id: "ann".into(),
            kind: FacetKind::Annotation,
            column_id: None,
            semantic: None,
            selection: include(&["starred"]),
            top_n: None,
            search: None,
            bins: None,
            pinned: false,
            collapsed: false,
            width: None,
        };
        // exclude-mode to also exercise that path is not required; keep include.
        ann.selection.mode = FacetMode::Include;
        let cfg = FacetConfig {
            facets: vec![sem, ann],
        };
        let conv = to_filter_group(&d, &cfg);
        let ids: HashSet<&str> = conv.dropped.iter().map(|d| d.id.as_str()).collect();
        assert!(ids.contains("sem"));
        assert!(ids.contains("ann"));
        assert!(conv.filter.nodes.is_empty());
    }

    #[test]
    fn nullability_blank_converts_to_is_empty() {
        // The second column keeps the blank-cell row from being an all-empty
        // line, which the parser would skip entirely.
        let d = doc("a,b\nx,1\n,2\ny,3\n");
        let mut s = spec("nb", FacetKind::Nullability, "c0");
        s.selection = include(&["blank"]);
        let cfg = FacetConfig { facets: vec![s] };
        let conv = to_filter_group(&d, &cfg);
        assert!(conv.dropped.is_empty());
        let via_filter = crate::filter::matching_rows(&d, &conv.filter).unwrap();
        let via_facets = matching_rows(&d, &cfg, &FacetInputs::default()).unwrap();
        assert_eq!(via_filter, via_facets);
        assert_eq!(via_facets, vec![1]); // only the genuinely blank row
    }

    #[test]
    fn unresolved_column_facet_never_filters() {
        let d = sample_doc();
        let cfg = FacetConfig {
            facets: vec![spec("gone", FacetKind::Text, "c99")], // no such column id
        };
        let rs = compute(&d, &cfg, &FacetInputs::default(), None).unwrap();
        let f = find(&rs, "gone");
        assert!(f.unresolved);
        assert!(!f.active);
        assert_eq!(rs.matched_rows, 5);
        assert_eq!(
            matching_rows(&d, &cfg, &FacetInputs::default()).unwrap(),
            vec![0, 1, 2, 3, 4]
        );
    }

    // ----- annotation facet integration (F40, wired for real) --------------

    fn annotated_doc() -> (Document, FacetInputs) {
        // 5 rows so one row carries no annotation.
        let d = doc("id,name\n1,Ada\n2,Bob\n3,Cy\n4,Di\n5,Ed\n");
        let mut store = AnnotationStore::default();
        let s = DocumentSource::new(&d);
        // Row 0: star + tag keep. Row 1: flag. Row 2: star. Row 3: tag keep.
        store
            .edit_row_marks(
                &s,
                0,
                &RowMarkPatch {
                    star: Some(true),
                    add_tags: vec!["keep".into()],
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        store
            .edit_row_marks(
                &s,
                1,
                &RowMarkPatch {
                    flag: Some(true),
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        store
            .edit_row_marks(
                &s,
                2,
                &RowMarkPatch {
                    star: Some(true),
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        store
            .edit_row_marks(
                &s,
                3,
                &RowMarkPatch {
                    add_tags: vec!["keep".into()],
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        let index = store.mark_index(&s, None).unwrap();
        let inputs = FacetInputs {
            annotation: Some(StatusInput::from_marks(&index)),
            ..Default::default()
        };
        (d, inputs)
    }

    #[test]
    fn annotation_facet_counts_marks() {
        let (d, inputs) = annotated_doc();
        let cfg = FacetConfig {
            facets: vec![FacetSpec {
                id: "ann".into(),
                kind: FacetKind::Annotation,
                column_id: None,
                semantic: None,
                selection: FacetSelection::default(),
                top_n: None,
                search: None,
                bins: None,
                pinned: false,
                collapsed: false,
                width: None,
            }],
        };
        let rs = compute(&d, &cfg, &inputs, None).unwrap();
        let f = find(&rs, "ann");
        assert_eq!(count(f, "starred"), 2); // rows 0,2
        assert_eq!(count(f, "flagged"), 1); // row 1
        assert_eq!(count(f, "tag:keep"), 2); // rows 0,3
        assert_eq!(count(f, NONE_KEY), 1); // row 4 carries nothing
    }

    #[test]
    fn annotation_facet_filters_and_cross_filters() {
        let (d, inputs) = annotated_doc();
        let mut ann = FacetSpec {
            id: "ann".into(),
            kind: FacetKind::Annotation,
            column_id: None,
            semantic: None,
            selection: include(&["starred"]),
            top_n: None,
            search: None,
            bins: None,
            pinned: false,
            collapsed: false,
            width: None,
        };
        ann.selection.mode = FacetMode::Include;
        let cfg = FacetConfig {
            facets: vec![ann, spec("name", FacetKind::Text, "c1")],
        };
        // Applying the annotation facet keeps exactly the starred rows.
        assert_eq!(matching_rows(&d, &cfg, &inputs).unwrap(), vec![0, 2]);

        // The text facet's counts reflect the annotation selection.
        let rs = compute(&d, &cfg, &inputs, None).unwrap();
        assert_eq!(rs.matched_rows, 2);
        let name = find(&rs, "name");
        assert_eq!(count(name, "Ada"), 1);
        assert_eq!(count(name, "Cy"), 1);
        assert!(name.buckets.iter().all(|b| b.key != "Bob")); // Bob (row1) filtered out
    }

    #[test]
    fn status_none_bucket_is_selectable() {
        let (d, inputs) = annotated_doc();
        let mut ann = FacetSpec {
            id: "ann".into(),
            kind: FacetKind::Annotation,
            column_id: None,
            semantic: None,
            selection: include(&[NONE_KEY]),
            top_n: None,
            search: None,
            bins: None,
            pinned: false,
            collapsed: false,
            width: None,
        };
        ann.selection.mode = FacetMode::Include;
        let cfg = FacetConfig { facets: vec![ann] };
        // Only row 4 has no annotation.
        assert_eq!(matching_rows(&d, &cfg, &inputs).unwrap(), vec![4]);
    }

    // ----- self-review regression tests ------------------------------------

    fn ann_facet(id: &str) -> FacetSpec {
        FacetSpec {
            id: id.into(),
            kind: FacetKind::Annotation,
            column_id: None,
            semantic: None,
            selection: FacetSelection::default(),
            top_n: None,
            search: None,
            bins: None,
            pinned: false,
            collapsed: false,
            width: None,
        }
    }

    #[test]
    fn truncated_scan_flags_status_facets_sampled() {
        // A truncated scan (as an indexed document over FACET_SAMPLE_ROWS gets)
        // under-counts status facets exactly like value facets, so their per-facet
        // `sampled` flag — the one that drives the UI "≈" estimate mark — must be
        // set, not just the top-level flag. `compute_scan` forces the sampled path
        // without a 200k-row indexed fixture.
        let (d, inputs) = annotated_doc(); // 5 rows
        let cfg = FacetConfig {
            facets: vec![ann_facet("ann"), spec("name", FacetKind::Text, "c1")],
        };
        // Full scan: nothing sampled.
        let full = compute_scan(&d, &cfg, &inputs, None, d.n_rows()).unwrap();
        assert!(!full.sampled);
        assert!(!find(&full, "ann").sampled);
        assert!(!find(&full, "name").sampled);

        // Truncated to the leading 3 rows: everything is an estimate.
        let rs = compute_scan(&d, &cfg, &inputs, None, 3).unwrap();
        assert!(rs.sampled);
        assert_eq!(rs.scanned_rows, 3);
        assert!(
            find(&rs, "ann").sampled,
            "status facet must flag sampled when its counts are truncated"
        );
        assert!(find(&rs, "name").sampled);
    }

    #[test]
    fn stale_diagnostics_issue_degrades_instead_of_failing() {
        // A diagnostics report cached before a column delete can name a column
        // that no longer exists ("whitespace:9"). `from_diagnostics` must skip it
        // (like F42) rather than propagate `issue_rows`' out-of-range error and
        // fail every other facet in the same compute call.
        use crate::diagnostics::{DiagnosticIssue, DiagnosticsReport, Severity};
        let d = sample_doc(); // 3 columns
        let stale = DiagnosticIssue {
            id: "whitespace:9".into(),
            kind: "whitespace".into(),
            severity: Severity::Warning,
            title: "Edge whitespace".into(),
            description: String::new(),
            affected_count: 0,
            samples: Vec::new(),
            suggested_action: None,
            row_filterable: true,
        };
        let report = DiagnosticsReport {
            doc_id: 1,
            revision: d.revision(),
            source: Vec::new(),
            current: vec![stale],
        };
        let input = StatusInput::from_diagnostics(&d, &report).expect("stale issue must not error");
        // The dead category is dropped; the facet degrades to just its none bucket.
        assert!(input.categories.is_empty());
        assert_eq!(input.none_label.as_deref(), Some("No issues"));
    }

    #[test]
    fn stale_crossval_rule_degrades_instead_of_failing() {
        // A cached cross-column rule referencing a renamed/deleted column makes
        // `violating_rows` error on name resolution; `from_validation` must swallow
        // that (like F42) and still return a usable validation facet.
        use crate::crossval::CrossRule;
        let d = sample_doc();
        let rules = vec![CrossRule::ExactlyOne {
            columns: vec!["ghost_a".into(), "ghost_b".into()],
        }];
        let input = StatusInput::from_validation(&d, &rules, d.schema_issues())
            .expect("stale rule must not error");
        // Cross-val category present but degraded to empty; schema category always.
        let crossval = input
            .categories
            .iter()
            .find(|c| c.key == "crossval")
            .expect("crossval category present");
        assert!(crossval.rows.is_empty());
        assert!(input.categories.iter().any(|c| c.key == "schema"));
    }

    #[test]
    fn selected_text_buckets_are_bounded() {
        // A hand-edited / persisted saved view with a giant selection.values array
        // must not turn into an unbounded bucket payload: the always-shown selected
        // buckets are capped at MAX_TEXT_VALUES (plus at most top_n "rest" buckets).
        let d = sample_doc();
        let mut s = spec("city", FacetKind::Text, "c0");
        let values: Vec<String> = (0..MAX_TEXT_VALUES + 500)
            .map(|i| format!("v{i}"))
            .collect();
        s.selection = FacetSelection {
            mode: FacetMode::Include,
            values,
            range: FacetRange::default(),
        };
        s.top_n = Some(20);
        let cfg = FacetConfig { facets: vec![s] };
        let rs = compute(&d, &cfg, &FacetInputs::default(), None).unwrap();
        let f = find(&rs, "city");
        assert!(
            f.buckets.len() <= MAX_TEXT_VALUES + 20,
            "selected buckets must stay bounded, got {}",
            f.buckets.len()
        );
    }
}
