//! Reproducible sampling and partitioning (F48): carve deterministic subsets
//! and disjoint splits out of any [`TabularSource`] without ever deleting a
//! source row. Every operation is driven by an explicit seed (supplied or
//! crypto-generated and surfaced), so the same source + settings + seed always
//! produces byte-identical outputs.
//!
//! ## What this module owns
//!
//! * A small, documented, seeded PRNG — SplitMix64 (seed expansion) feeding
//!   xoshiro256** (the sampling stream). No `thread_rng`: sampling must be
//!   reproducible, so the RNG state is a pure function of the seed. Test
//!   vectors below pin the algorithm.
//! * Eight sampling methods: head (`first N`), tail (`last N`), random fixed
//!   count (reservoir — Algorithm R, single pass, bounded memory, works over
//!   an unseekable/indexed source), random percentage (independent Bernoulli),
//!   systematic every-Nth with offset, stratified (proportional, tolerance
//!   reported), balanced (equal per stratum, shortfall reported), and
//!   hash-based deterministic (stable content/key hash mod threshold).
//! * Partitioning into N weighted, named outputs (train/validation/test and
//!   custom), optionally stratified, optionally group-preserving (rows sharing
//!   key-column values never split across partitions — assigned by weighted
//!   hash of the group key, with the resulting weight skew reported). Non-group
//!   partitions use exact largest-remainder counts. Every partition is disjoint
//!   by construction.
//! * A preview DTO (projected AND exact counts per output, plus a strata table
//!   when stratifying), job-wrapped execution with cancellation, outputs to
//!   NEW derived documents OR direct CSV exports, and a JSON manifest
//!   (method, seed, source fingerprint, scope, per-output counts + SHA-256).
//!
//! ## Streaming & memory
//!
//! Selection over the `All` scope streams the source in bounded windows and
//! never materialises row DATA: only the reservoir (bounded to `n` row indices)
//! or a set of selected 8-byte row indices is held — the row bytes are read
//! again at write time and spilled to disk by the derived-document builder.
//! Reservoir sampling is strictly single-pass and O(n). Stratified methods take
//! a documented second pass (grouping then allocating) — unavoidable, since
//! proportional allocation needs each stratum's size before it can select.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::derived::DerivedDocumentBuilder;
use crate::document::Document;
use crate::dto::{BackupPolicy, ExportOptions};
use crate::error::{AppError, AppResult};
use crate::job::JobCtx;
use crate::row_identity::{composite_key, row_content_hash, CompositeKey, KeyNormalization};
use crate::tabular::{
    ContentFingerprint, CsvSink, TabularColumn, TabularRow, TabularSink, TabularSource,
    DEFAULT_WINDOW,
};

// =====================================================================================
// Seeded PRNG
// =====================================================================================

/// SplitMix64: a 64-bit state PRNG used only to expand a single user seed into
/// the four words xoshiro256** needs. Canonical constants (Vigna); the seed=0
/// reference output is pinned in the tests.
#[derive(Debug, Clone)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> SplitMix64 {
        SplitMix64 { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

/// xoshiro256** — the sampling PRNG. Small, fast, and reproducible: state is a
/// pure function of the seed, so every draw is deterministic. Not cryptographic
/// (sampling does not need that); the seed itself IS crypto-random when
/// generated ([`resolve_seed`]).
#[derive(Debug, Clone)]
pub struct Prng {
    s: [u64; 4],
}

impl Prng {
    /// Seed the generator, expanding the single 64-bit seed through SplitMix64.
    pub fn seed(seed: u64) -> Prng {
        let mut sm = SplitMix64::new(seed);
        Prng {
            s: [sm.next_u64(), sm.next_u64(), sm.next_u64(), sm.next_u64()],
        }
    }

    /// Derive an INDEPENDENT sub-stream from a base seed and a purpose-specific
    /// salt, so (for example) the reservoir stream and each stratum's shuffle
    /// stream never share state yet both stay a pure function of the base seed.
    fn derive(seed: u64, salt: u64) -> Prng {
        // Mix the salt through SplitMix64 before combining, so adjacent salts
        // (0, 1, 2 … stratum ordinals) yield well-separated streams.
        let mixed = SplitMix64::new(salt).next_u64();
        Prng::seed(seed ^ mixed)
    }

    pub fn next_u64(&mut self) -> u64 {
        let result = self.s[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.s[1] << 17;
        self.s[2] ^= self.s[0];
        self.s[3] ^= self.s[1];
        self.s[1] ^= self.s[2];
        self.s[0] ^= self.s[3];
        self.s[2] ^= t;
        self.s[3] = self.s[3].rotate_left(45);
        result
    }

    /// Uniform integer in `[0, bound)` with no modulo bias (Lemire). `bound`
    /// must be positive; `0` yields `0`.
    pub fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            return 0;
        }
        let mut x = self.next_u64();
        let mut m = (x as u128).wrapping_mul(bound as u128);
        let mut low = m as u64;
        if low < bound {
            let threshold = bound.wrapping_neg() % bound;
            while low < threshold {
                x = self.next_u64();
                m = (x as u128).wrapping_mul(bound as u128);
                low = m as u64;
            }
        }
        (m >> 64) as u64
    }

    /// Uniform float in `[0, 1)` (53 bits of precision).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / ((1u64 << 53) as f64))
    }

    /// In-place Fisher-Yates shuffle.
    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below((i + 1) as u64) as usize;
            items.swap(i, j);
        }
    }
}

/// Salt constants that keep the derived sub-streams disjoint by purpose.
mod salt {
    pub const RESERVOIR: u64 = 0x01;
    pub const BERNOULLI: u64 = 0x02;
    pub const SYSTEMATIC_OFFSET: u64 = 0x03;
    pub const PARTITION_LABELS: u64 = 0x04;
    /// Base for per-output ordering streams; the output ordinal is added.
    pub const ORDER_BASE: u64 = 0x1000;
    /// Base for per-stratum selection/allocation streams; the ordinal is added.
    pub const STRATUM_BASE: u64 = 0x2000;
}

/// Resolve the effective seed: use the caller's when supplied, otherwise draw a
/// crypto-random one (surfaced back to the user for reproducibility).
pub fn resolve_seed(seed: Option<u64>) -> AppResult<u64> {
    match seed {
        Some(s) => Ok(s),
        None => {
            let mut bytes = [0u8; 8];
            getrandom::getrandom(&mut bytes)
                .map_err(|e| AppError::Other(format!("secure random unavailable: {e}")))?;
            Ok(u64::from_le_bytes(bytes))
        }
    }
}

// =====================================================================================
// Request / response DTOs
// =====================================================================================

/// Which rows the operation draws from: the whole source, or only the rows
/// currently visible under the active filter/view (F04/F12 scope).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SampleScope {
    #[default]
    All,
    VisibleRows,
}

/// Emit outputs in source order, or in a seeded shuffle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SampleOrder {
    /// Ascending source order (stable, streamable).
    #[default]
    SourceOrder,
    /// Seeded shuffle of the selected rows.
    Shuffle,
}

/// One of the eight sampling methods. Columns are addressed by STABLE column id
/// (F12), like [`crate::row_identity::KeySpec`], so a saved method survives
/// renames and reorders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SamplingMethod {
    /// The first `n` rows.
    Head { n: u64 },
    /// The last `n` rows.
    Tail { n: u64 },
    /// A uniform random sample of exactly `min(n, total)` rows, drawn by
    /// reservoir sampling (single pass, bounded memory).
    RandomCount { n: u64 },
    /// Each row kept independently with probability `percent`% (Bernoulli); the
    /// realised count varies around `percent`% of the rows.
    RandomPercentage { percent: f64 },
    /// Every `step`-th row, starting at `offset` (a random offset in
    /// `[0, step)` is drawn from the seed when `offset` is omitted).
    Systematic { step: u64, offset: Option<u64> },
    /// Proportional stratified sampling: within each stratum (a distinct tuple
    /// of the key columns) keep `fraction` of the rows. `tolerance` is the
    /// allowed absolute gap between the realised overall fraction and
    /// `fraction` before a warning is raised.
    Stratified {
        columns: Vec<String>,
        fraction: f64,
        #[serde(default)]
        tolerance: f64,
    },
    /// Balanced stratified sampling: keep exactly `per_stratum` rows from each
    /// stratum; strata with fewer rows contribute all of theirs and are
    /// reported as shortfalls.
    Balanced {
        columns: Vec<String>,
        per_stratum: u64,
    },
    /// Deterministic hash-based sampling: keep a row when the seeded hash of its
    /// content (or of `columns`, when given) falls under `percent`% — a stable
    /// subset that does not move when unrelated rows are added or removed.
    HashDeterministic {
        #[serde(default)]
        columns: Option<Vec<String>>,
        percent: f64,
    },
}

/// One partition of a split: a name (e.g. "train") and a relative weight.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartitionOutput {
    pub name: String,
    pub weight: f64,
}

/// A split into N disjoint, weighted, named partitions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartitionSpec {
    pub parts: Vec<PartitionOutput>,
    /// Stratify the split by these columns (empty = no stratification): each
    /// partition then holds each stratum in proportion to its weight.
    #[serde(default)]
    pub stratify_by: Vec<String>,
    /// Keep rows sharing these key-column values together (empty = per-row):
    /// whole groups are assigned to a partition by weighted hash, so a group is
    /// never split. Mutually exclusive with `stratify_by`.
    #[serde(default)]
    pub group_by: Vec<String>,
    /// Partitions are disjoint unless this is set. Overlapping partitions are
    /// not yet implemented; setting this is rejected (reserved for a future
    /// bootstrap-style mode).
    #[serde(default)]
    pub allow_overlap: bool,
}

/// A sampling operation OR a partitioning operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SamplePlan {
    Sampling(SamplingMethod),
    Partitioning(PartitionSpec),
}

/// Where the outputs land: new in-app documents, or CSV files on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SampleDestination {
    /// Each output becomes a NEW derived document (F20 builder: editable when
    /// small, indexed read-only when it spills).
    DerivedDocuments,
    /// Each output is written as a CSV file `<dir>/<base_name>-<output>.csv`
    /// (a single output keeps `<base_name>.csv`), plus an optional manifest.
    Export {
        dir: String,
        base_name: String,
        options: ExportOptions,
        #[serde(default)]
        write_manifest: bool,
    },
}

/// A full sampling/partitioning request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleRequest {
    pub plan: SamplePlan,
    #[serde(default)]
    pub scope: SampleScope,
    #[serde(default)]
    pub order: SampleOrder,
    /// The seed. `None` in a preview draws a fresh crypto-random seed (returned
    /// in the preview); pass that value back on run for reproducibility.
    #[serde(default)]
    pub seed: Option<u64>,
    pub destination: SampleDestination,
}

/// Projected vs. exact row count for one output.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputProjection {
    pub name: String,
    /// The count the method's formula predicts (before running).
    pub projected: u64,
    /// The count the deterministic selection actually produces.
    pub exact: u64,
}

/// One stratum's population and selection, for the preview's strata table.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StratumRow {
    /// The stratum's key cell values (missing cells render as empty).
    pub key: Vec<String>,
    pub population: u64,
    pub selected: u64,
    pub fraction: f64,
}

/// Non-binding preview of a sampling/partitioning run: the resolved seed, the
/// scope size, per-output projected + exact counts, an optional strata table,
/// and any warnings (tolerance exceeded, balanced shortfalls, group skew).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SamplePreview {
    pub seed: u64,
    pub source_fingerprint: String,
    pub scope: SampleScope,
    pub order: SampleOrder,
    pub total_rows: u64,
    pub outputs: Vec<OutputProjection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strata: Option<Vec<StratumRow>>,
    pub warnings: Vec<String>,
    /// Document revision this preview was computed against; the run rejects a
    /// mismatch.
    pub expected_revision: u64,
}

/// One output recorded in a manifest.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleManifestOutput {
    pub name: String,
    /// File name for exports; `None` for a derived-document output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    pub rows: u64,
    /// SHA-256 of the file bytes (exports) or of the canonical row content
    /// stream (derived documents).
    pub sha256: String,
}

/// The manifest for a completed run.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleManifest {
    /// Machine-readable method label (e.g. "randomCount", "partition").
    pub method: String,
    pub seed: u64,
    pub source_fingerprint: String,
    pub scope: SampleScope,
    pub order: SampleOrder,
    pub total_rows: u64,
    pub outputs: Vec<SampleManifestOutput>,
}

/// Handles returned by `start_sample`: the job to watch, plus the ids the run
/// will register (empty for a direct export).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleStart {
    pub job_id: u64,
    pub doc_ids: Vec<u64>,
}

// =====================================================================================
// Universe (the rows in scope) + streaming readers
// =====================================================================================

/// The set of source rows an operation draws from, in scope order: either all
/// `n` rows (implicit `0..n`, never materialised) or an explicit ascending
/// index list (the active filter view).
#[derive(Debug, Clone)]
pub enum Universe {
    All(u64),
    Indices(Vec<u64>),
}

impl Universe {
    // An internal read cursor: "length" is the scope size; emptiness is not a
    // meaningful public concept, so no is_empty companion.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> u64 {
        match self {
            Universe::All(n) => *n,
            Universe::Indices(v) => v.len() as u64,
        }
    }
}

/// Read a contiguous absolute range `[start, start+count)` in bounded windows,
/// invoking `f(abs_index, row)`. Stops early if `f` returns `Ok(false)`.
fn read_range(
    source: &dyn TabularSource,
    start: u64,
    count: u64,
    ctx: Option<&JobCtx>,
    f: &mut dyn FnMut(u64, &TabularRow) -> AppResult<bool>,
) -> AppResult<()> {
    let mut read = 0u64;
    while read < count {
        if let Some(ctx) = ctx {
            ctx.check()?;
        }
        let want = (count - read).min(DEFAULT_WINDOW as u64) as usize;
        let rows = source.read_rows(start + read, want, ctx)?;
        if rows.is_empty() {
            break;
        }
        for (k, row) in rows.iter().enumerate() {
            if !f(start + read + k as u64, row)? {
                return Ok(());
            }
        }
        let got = rows.len() as u64;
        read += got;
        if (got as usize) < want {
            break;
        }
    }
    Ok(())
}

/// Read an explicit list of absolute indices IN THE GIVEN ORDER, batching
/// consecutive runs into one windowed read. Non-consecutive indices (a shuffle,
/// a sparse filter view) fall back to short reads — random access, which the
/// document and indexed backings both support.
fn read_indices(
    source: &dyn TabularSource,
    indices: &[u64],
    ctx: Option<&JobCtx>,
    f: &mut dyn FnMut(u64, &TabularRow) -> AppResult<bool>,
) -> AppResult<()> {
    let mut i = 0usize;
    while i < indices.len() {
        if let Some(ctx) = ctx {
            ctx.check()?;
        }
        let start = indices[i];
        let mut run = 1usize;
        while i + run < indices.len() && indices[i + run] == start + run as u64 {
            run += 1;
        }
        let mut keep_going = true;
        read_range(source, start, run as u64, ctx, &mut |abs, row| {
            keep_going = f(abs, row)?;
            Ok(keep_going)
        })?;
        if !keep_going {
            return Ok(());
        }
        i += run;
    }
    Ok(())
}

/// Stream every row in `universe`, invoking `f(pos, abs_index, row)` where
/// `pos` is the position within the scope.
fn scan_universe(
    source: &dyn TabularSource,
    universe: &Universe,
    ctx: Option<&JobCtx>,
    mut f: impl FnMut(usize, u64, &TabularRow) -> AppResult<bool>,
) -> AppResult<()> {
    match universe {
        Universe::All(n) => {
            let mut pos = 0usize;
            read_range(source, 0, *n, ctx, &mut |abs, row| {
                let go = f(pos, abs, row)?;
                pos += 1;
                Ok(go)
            })
        }
        Universe::Indices(v) => {
            let mut pos = 0usize;
            read_indices(source, v, ctx, &mut |abs, row| {
                let go = f(pos, abs, row)?;
                pos += 1;
                Ok(go)
            })
        }
    }
}

// =====================================================================================
// Reservoir (Algorithm R) — single-pass, bounded to the target size
// =====================================================================================

/// Uniform reservoir sampler over a stream of items, holding at most `capacity`
/// of them at any time (Algorithm R). Deterministic given its [`Prng`].
#[derive(Debug)]
pub struct Reservoir<T> {
    capacity: usize,
    seen: u64,
    buf: Vec<T>,
    rng: Prng,
}

impl<T> Reservoir<T> {
    fn new(capacity: usize, rng: Prng) -> Reservoir<T> {
        Reservoir {
            capacity,
            seen: 0,
            buf: Vec::with_capacity(capacity),
            rng,
        }
    }

    fn add(&mut self, item: T) {
        if self.capacity == 0 {
            self.seen += 1;
            return;
        }
        if self.buf.len() < self.capacity {
            self.buf.push(item);
        } else {
            // Replace a random existing slot with probability capacity/seen.
            let j = self.rng.below(self.seen + 1) as usize;
            if j < self.capacity {
                self.buf[j] = item;
            }
        }
        self.seen += 1;
    }

    fn into_inner(self) -> Vec<T> {
        self.buf
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.buf.len()
    }

    #[cfg(test)]
    fn allocated(&self) -> usize {
        self.buf.capacity()
    }
}

// =====================================================================================
// Hashing (hash-based sampling + group-preserving assignment)
// =====================================================================================

/// A stable 64-bit hash of a row's subject cells, folded with the seed so a
/// different seed yields a different (still deterministic) subset. Reuses the
/// boundary-safe [`row_content_hash`], so `["ab","c"]`, `["a","bc"]`, and
/// missing-vs-empty cells all hash differently.
fn seeded_hash64(seed: u64, subject: &[Option<String>]) -> u64 {
    let mut h = Sha256::new();
    h.update(seed.to_le_bytes());
    h.update(row_content_hash(subject));
    let digest = h.finalize();
    u64::from_le_bytes(digest[0..8].try_into().expect("sha256 yields 32 bytes"))
}

/// Extract the subject cells for hashing: the key columns when given, else the
/// whole row.
fn subject_cells(row: &TabularRow, positions: Option<&[usize]>) -> Vec<Option<String>> {
    match positions {
        None => row.clone(),
        Some(pos) => pos
            .iter()
            .map(|&p| row.get(p).and_then(Clone::clone))
            .collect(),
    }
}

// =====================================================================================
// Column resolution & percentage helpers
// =====================================================================================

/// Resolve stable column ids to positions in the source schema (by id, never by
/// header text — a spec survives renames and reorders).
fn resolve_columns(cols: &[TabularColumn], ids: &[String]) -> AppResult<Vec<usize>> {
    if ids.is_empty() {
        return Err(AppError::invalid("pick at least one column"));
    }
    ids.iter()
        .map(|id| {
            cols.iter()
                .position(|c| c.id.as_deref() == Some(id.as_str()))
                .ok_or_else(|| {
                    AppError::invalid(format!("column '{id}' does not exist in the source"))
                })
        })
        .collect()
}

fn validate_percent(percent: f64) -> AppResult<()> {
    if !percent.is_finite() || !(0.0..=100.0).contains(&percent) {
        return Err(AppError::invalid("percentage must be between 0 and 100"));
    }
    Ok(())
}

fn validate_fraction(fraction: f64) -> AppResult<()> {
    if !fraction.is_finite() || !(0.0..=1.0).contains(&fraction) {
        return Err(AppError::invalid("fraction must be between 0 and 1"));
    }
    Ok(())
}

/// The hash-bucket threshold (out of 10 000) for a percentage. A row is kept
/// when `hash % 10_000 < threshold`.
fn percent_threshold(percent: f64) -> u64 {
    ((percent * 100.0).round() as i64).clamp(0, 10_000) as u64
}

/// Render a source fingerprint into a stable, comparable string for manifests.
pub fn render_fingerprint(fp: ContentFingerprint) -> String {
    match fp {
        ContentFingerprint::File(f) => format!("file:{}:{}", f.size, f.modified_at_ms),
        ContentFingerprint::Revision { doc_id, revision } => format!("rev:{doc_id}:{revision}"),
        ContentFingerprint::Unknown => "unknown".to_string(),
    }
}

/// Largest-remainder apportionment of `total` across `weights`: the parts sum to
/// exactly `total`, remainders broken deterministically (largest first, then
/// lowest index).
fn largest_remainder(total: u64, weights: &[f64]) -> Vec<u64> {
    let sum: f64 = weights.iter().sum();
    if sum <= 0.0 {
        return vec![0; weights.len()];
    }
    let mut base = Vec::with_capacity(weights.len());
    let mut remainders: Vec<(f64, usize)> = Vec::with_capacity(weights.len());
    let mut assigned = 0u64;
    for (i, &w) in weights.iter().enumerate() {
        let exact = total as f64 * (w.max(0.0)) / sum;
        let floor = exact.floor();
        base.push(floor as u64);
        remainders.push((exact - floor, i));
        assigned += floor as u64;
    }
    let mut leftover = total.saturating_sub(assigned);
    remainders.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    for &(_, i) in &remainders {
        if leftover == 0 {
            break;
        }
        base[i] += 1;
        leftover -= 1;
    }
    base
}

// =====================================================================================
// The plan: selected outputs (lists of absolute source indices)
// =====================================================================================

/// One output's plan: its name and the absolute source indices it will contain,
/// already in emission order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPlan {
    pub name: String,
    pub indices: Vec<u64>,
}

/// The full result of planning: the outputs, an optional strata table, and any
/// warnings surfaced to the user.
#[derive(Debug, Clone)]
pub struct PlanResult {
    pub outputs: Vec<OutputPlan>,
    pub strata: Option<Vec<StratumRow>>,
    pub warnings: Vec<String>,
    /// Machine-readable method label for the manifest.
    pub method_label: String,
}

/// Apply the requested emission order to a selected index list.
fn apply_order(indices: &mut [u64], order: SampleOrder, seed: u64, ordinal: u64) {
    match order {
        SampleOrder::SourceOrder => indices.sort_unstable(),
        SampleOrder::Shuffle => {
            // Sort first so the shuffle input is deterministic regardless of the
            // selection algorithm's internal ordering, then shuffle.
            indices.sort_unstable();
            let mut rng = Prng::derive(seed, salt::ORDER_BASE + ordinal);
            rng.shuffle(indices);
        }
    }
}

/// Hard cap on the number of distinct strata / groups a stratify-or-group-by key
/// may produce. Mirrors the bounded-grouping guards elsewhere in the core
/// ([`crate::groupby::MAX_GROUPS`], [`crate::cluster::MAX_DISTINCT_VALUES`]): a
/// near-unique key column (an id, an email) would otherwise grow an unbounded
/// `HashMap` and — for stratified/balanced sampling, whose strata table is
/// serialized to the UI on every preview — an unbounded IPC payload. Set to
/// cluster's 200 000, the tighter of the two precedents, because that IPC path
/// is the binding constraint here.
const MAX_STRATA: usize = 200_000;

/// Strata/groups: the keys in first-seen order, plus each key's member rows
/// (absolute indices, in source order).
type KeyGroups = (Vec<CompositeKey>, HashMap<CompositeKey, Vec<u64>>);

/// Group scope positions by a composite stratum/group key, preserving
/// first-seen key order. Returns `(ordered_keys, key -> abs indices)`. The
/// distinct-key count is bounded by [`MAX_STRATA`]; a higher-cardinality key
/// fails fast rather than growing memory (and the IPC strata payload) without
/// bound.
fn group_by_key(
    source: &dyn TabularSource,
    universe: &Universe,
    positions: &[usize],
    ctx: Option<&JobCtx>,
) -> AppResult<KeyGroups> {
    let norm = KeyNormalization::default();
    let mut order: Vec<CompositeKey> = Vec::new();
    let mut groups: HashMap<CompositeKey, Vec<u64>> = HashMap::new();
    scan_universe(source, universe, ctx, |_pos, abs, row| {
        let key = composite_key(row, positions, &norm);
        match groups.get_mut(&key) {
            Some(entry) => entry.push(abs),
            None => {
                if groups.len() >= MAX_STRATA {
                    return Err(AppError::invalid(format!(
                        "more than {MAX_STRATA} distinct strata/groups — stratify or \
                         group by fewer or coarser columns"
                    )));
                }
                order.push(key.clone());
                groups.insert(key, vec![abs]);
            }
        }
        Ok(true)
    })?;
    Ok((order, groups))
}

fn render_key(key: &CompositeKey) -> Vec<String> {
    key.0
        .iter()
        .map(|c| c.clone().unwrap_or_default())
        .collect()
}

/// Plan a sampling operation over `universe`.
pub fn plan_sample(
    source: &dyn TabularSource,
    universe: &Universe,
    method: &SamplingMethod,
    order: SampleOrder,
    seed: u64,
    ctx: Option<&JobCtx>,
) -> AppResult<PlanResult> {
    let total = universe.len();
    let mut warnings = Vec::new();
    let mut strata: Option<Vec<StratumRow>> = None;

    let (mut indices, method_label): (Vec<u64>, &str) = match method {
        SamplingMethod::Head { n } => {
            let take = (*n).min(total);
            let mut out = Vec::with_capacity(take as usize);
            scan_universe(source, universe, ctx, |pos, abs, _row| {
                if (pos as u64) < take {
                    out.push(abs);
                    Ok((pos as u64 + 1) < take)
                } else {
                    Ok(false)
                }
            })?;
            (out, "head")
        }
        SamplingMethod::Tail { n } => {
            let take = (*n).min(total) as usize;
            // Bounded ring buffer of the last `take` absolute indices.
            let mut ring: std::collections::VecDeque<u64> =
                std::collections::VecDeque::with_capacity(take);
            scan_universe(source, universe, ctx, |_pos, abs, _row| {
                if take == 0 {
                    return Ok(true);
                }
                if ring.len() == take {
                    ring.pop_front();
                }
                ring.push_back(abs);
                Ok(true)
            })?;
            (ring.into_iter().collect(), "tail")
        }
        SamplingMethod::RandomCount { n } => {
            let cap = (*n).min(total) as usize;
            let mut reservoir: Reservoir<u64> =
                Reservoir::new(cap, Prng::derive(seed, salt::RESERVOIR));
            scan_universe(source, universe, ctx, |_pos, abs, _row| {
                reservoir.add(abs);
                Ok(true)
            })?;
            (reservoir.into_inner(), "randomCount")
        }
        SamplingMethod::RandomPercentage { percent } => {
            validate_percent(*percent)?;
            let p = *percent / 100.0;
            let mut rng = Prng::derive(seed, salt::BERNOULLI);
            let mut out = Vec::new();
            scan_universe(source, universe, ctx, |_pos, abs, _row| {
                // Draw for EVERY row (in scope order) so the stream stays a pure
                // function of the seed regardless of which rows are kept.
                if rng.next_f64() < p {
                    out.push(abs);
                }
                Ok(true)
            })?;
            (out, "randomPercentage")
        }
        SamplingMethod::Systematic { step, offset } => {
            if *step == 0 {
                return Err(AppError::invalid("step must be at least 1"));
            }
            let offset = match offset {
                Some(o) => *o,
                None => Prng::derive(seed, salt::SYSTEMATIC_OFFSET).below(*step),
            };
            let mut out = Vec::new();
            scan_universe(source, universe, ctx, |pos, abs, _row| {
                let p = pos as u64;
                if p >= offset && (p - offset).is_multiple_of(*step) {
                    out.push(abs);
                }
                Ok(true)
            })?;
            (out, "systematic")
        }
        SamplingMethod::Stratified {
            columns,
            fraction,
            tolerance,
        } => {
            validate_fraction(*fraction)?;
            let positions = resolve_columns(&source.columns(), columns)?;
            let (keys, groups) = group_by_key(source, universe, &positions, ctx)?;
            let mut out = Vec::new();
            let mut table = Vec::with_capacity(keys.len());
            let mut total_selected = 0u64;
            for (ordinal, key) in keys.iter().enumerate() {
                let members = &groups[key];
                let pop = members.len() as u64;
                let want = ((pop as f64) * *fraction).round() as u64;
                let want = want.min(pop);
                let mut idx = members.clone();
                let mut rng = Prng::derive(seed, salt::STRATUM_BASE + ordinal as u64);
                rng.shuffle(&mut idx);
                idx.truncate(want as usize);
                total_selected += want;
                out.extend_from_slice(&idx);
                table.push(StratumRow {
                    key: render_key(key),
                    population: pop,
                    selected: want,
                    fraction: if pop == 0 {
                        0.0
                    } else {
                        want as f64 / pop as f64
                    },
                });
            }
            let achieved = if total == 0 {
                0.0
            } else {
                total_selected as f64 / total as f64
            };
            if (achieved - *fraction).abs() > *tolerance {
                warnings.push(format!(
                    "realised fraction {achieved:.4} differs from target {:.4} by more than the \
                     tolerance {:.4}",
                    *fraction, *tolerance
                ));
            }
            strata = Some(table);
            (out, "stratified")
        }
        SamplingMethod::Balanced {
            columns,
            per_stratum,
        } => {
            let positions = resolve_columns(&source.columns(), columns)?;
            let (keys, groups) = group_by_key(source, universe, &positions, ctx)?;
            let mut out = Vec::new();
            let mut table = Vec::with_capacity(keys.len());
            for (ordinal, key) in keys.iter().enumerate() {
                let members = &groups[key];
                let pop = members.len() as u64;
                let want = (*per_stratum).min(pop);
                if want < *per_stratum {
                    warnings.push(format!(
                        "stratum {:?} has only {pop} row(s); short of the requested {per_stratum} \
                         by {}",
                        render_key(key),
                        *per_stratum - pop
                    ));
                }
                let mut idx = members.clone();
                let mut rng = Prng::derive(seed, salt::STRATUM_BASE + ordinal as u64);
                rng.shuffle(&mut idx);
                idx.truncate(want as usize);
                out.extend_from_slice(&idx);
                table.push(StratumRow {
                    key: render_key(key),
                    population: pop,
                    selected: want,
                    fraction: if pop == 0 {
                        0.0
                    } else {
                        want as f64 / pop as f64
                    },
                });
            }
            strata = Some(table);
            (out, "balanced")
        }
        SamplingMethod::HashDeterministic { columns, percent } => {
            validate_percent(*percent)?;
            let positions = match columns {
                Some(ids) => Some(resolve_columns(&source.columns(), ids)?),
                None => None,
            };
            let threshold = percent_threshold(*percent);
            let mut out = Vec::new();
            scan_universe(source, universe, ctx, |_pos, abs, row| {
                let subject = subject_cells(row, positions.as_deref());
                if seeded_hash64(seed, &subject) % 10_000 < threshold {
                    out.push(abs);
                }
                Ok(true)
            })?;
            (out, "hashDeterministic")
        }
    };

    apply_order(&mut indices, order, seed, 0);
    Ok(PlanResult {
        outputs: vec![OutputPlan {
            name: "sample".to_string(),
            indices,
        }],
        strata,
        warnings,
        method_label: method_label.to_string(),
    })
}

/// Plan a partitioning operation over `universe`.
pub fn plan_partition(
    source: &dyn TabularSource,
    universe: &Universe,
    spec: &PartitionSpec,
    order: SampleOrder,
    seed: u64,
    ctx: Option<&JobCtx>,
) -> AppResult<PlanResult> {
    if spec.allow_overlap {
        return Err(AppError::invalid(
            "overlapping partitions are not yet supported — partitions are disjoint",
        ));
    }
    if spec.parts.len() < 2 {
        return Err(AppError::invalid("a split needs at least two partitions"));
    }
    if spec
        .parts
        .iter()
        .any(|p| !p.weight.is_finite() || p.weight < 0.0)
    {
        return Err(AppError::invalid("partition weights must be non-negative"));
    }
    if spec.parts.iter().map(|p| p.weight).sum::<f64>() <= 0.0 {
        return Err(AppError::invalid(
            "at least one partition weight must be positive",
        ));
    }
    {
        let mut seen = std::collections::HashSet::new();
        for p in &spec.parts {
            if p.name.trim().is_empty() {
                return Err(AppError::invalid("partition names cannot be blank"));
            }
            if !seen.insert(p.name.as_str()) {
                return Err(AppError::invalid(format!(
                    "duplicate partition name '{}'",
                    p.name
                )));
            }
        }
    }
    if !spec.group_by.is_empty() && !spec.stratify_by.is_empty() {
        return Err(AppError::invalid(
            "group-preserving and stratified partitioning cannot be combined",
        ));
    }

    let weights: Vec<f64> = spec.parts.iter().map(|p| p.weight).collect();
    let total = universe.len();
    let mut warnings = Vec::new();
    let mut buckets: Vec<Vec<u64>> = vec![Vec::new(); spec.parts.len()];

    if !spec.group_by.is_empty() {
        // Group-preserving: whole groups assigned by weighted hash of the key.
        let positions = resolve_columns(&source.columns(), &spec.group_by)?;
        let (keys, groups) = group_by_key(source, universe, &positions, ctx)?;
        let cumulative = cumulative_fractions(&weights);
        for key in &keys {
            let members = &groups[key];
            let subject: Vec<Option<String>> = key.0.clone();
            // The group's partition is a pure function of its key + the seed, so
            // the group is never split and the mapping is reproducible.
            let h = seeded_hash64(seed, &subject);
            let u = (h >> 11) as f64 * (1.0 / ((1u64 << 53) as f64));
            let part = partition_for(u, &cumulative);
            buckets[part].extend_from_slice(members);
        }
        // Report the resulting weight skew (group sizes vary, so realised
        // fractions drift from the targets).
        report_skew(&buckets, &weights, total, &mut warnings);
    } else if !spec.stratify_by.is_empty() {
        // Stratified: each stratum split by exact largest-remainder counts.
        let positions = resolve_columns(&source.columns(), &spec.stratify_by)?;
        let (keys, groups) = group_by_key(source, universe, &positions, ctx)?;
        for (ordinal, key) in keys.iter().enumerate() {
            let members = &groups[key];
            let counts = largest_remainder(members.len() as u64, &weights);
            let mut rng = Prng::derive(seed, salt::STRATUM_BASE + ordinal as u64);
            assign_shuffled(members, &counts, &mut rng, &mut buckets);
        }
    } else {
        // Plain weighted split: exact counts over the whole scope.
        let counts = largest_remainder(total, &weights);
        let mut rng = Prng::derive(seed, salt::PARTITION_LABELS);
        // Materialise the scope's absolute indices in order, then assign.
        let mut abs_all = Vec::with_capacity(total as usize);
        scan_universe(source, universe, ctx, |_pos, abs, _row| {
            abs_all.push(abs);
            Ok(true)
        })?;
        assign_shuffled(&abs_all, &counts, &mut rng, &mut buckets);
    }

    let outputs = spec
        .parts
        .iter()
        .zip(buckets)
        .enumerate()
        .map(|(ordinal, (part, mut indices))| {
            apply_order(&mut indices, order, seed, ordinal as u64);
            OutputPlan {
                name: part.name.clone(),
                indices,
            }
        })
        .collect();

    Ok(PlanResult {
        outputs,
        strata: None,
        warnings,
        method_label: "partition".to_string(),
    })
}

/// Cumulative normalised weight boundaries in `[0, 1]`.
fn cumulative_fractions(weights: &[f64]) -> Vec<f64> {
    let sum: f64 = weights.iter().sum();
    let mut acc = 0.0;
    let mut out = Vec::with_capacity(weights.len());
    for &w in weights {
        acc += w.max(0.0) / sum;
        out.push(acc);
    }
    // Guard the final boundary against rounding so u==~1.0 always lands.
    if let Some(last) = out.last_mut() {
        *last = 1.0;
    }
    out
}

/// The partition whose cumulative boundary first exceeds `u` in `[0, 1)`.
fn partition_for(u: f64, cumulative: &[f64]) -> usize {
    for (i, &c) in cumulative.iter().enumerate() {
        if u < c {
            return i;
        }
    }
    cumulative.len() - 1
}

/// Assign `members` (in source order) to `buckets` by building the exact label
/// multiset from `counts`, shuffling it, and routing each member.
fn assign_shuffled(members: &[u64], counts: &[u64], rng: &mut Prng, buckets: &mut [Vec<u64>]) {
    let mut labels: Vec<u32> = Vec::with_capacity(members.len());
    for (p, &c) in counts.iter().enumerate() {
        for _ in 0..c {
            labels.push(p as u32);
        }
    }
    // `counts` sums to members.len() (largest_remainder guarantees it), so the
    // label multiset lines up one-to-one with the members.
    rng.shuffle(&mut labels);
    for (member, label) in members.iter().zip(labels.iter()) {
        buckets[*label as usize].push(*member);
    }
}

/// Push a warning per partition whose realised fraction drifts from its target
/// by more than one percentage point (group-preserving splits only).
fn report_skew(buckets: &[Vec<u64>], weights: &[f64], total: u64, warnings: &mut Vec<String>) {
    if total == 0 {
        return;
    }
    let sum: f64 = weights.iter().sum();
    for (i, bucket) in buckets.iter().enumerate() {
        let target = weights[i] / sum;
        let realised = bucket.len() as f64 / total as f64;
        if (realised - target).abs() > 0.01 {
            warnings.push(format!(
                "partition {i} weight skew: target {target:.4}, realised {realised:.4} \
                 (group sizes vary, so group-preserving splits are approximate)"
            ));
        }
    }
}

// =====================================================================================
// Preview
// =====================================================================================

/// Projected counts for a sampling method (the formula, before running).
fn project_sample(method: &SamplingMethod, total: u64) -> u64 {
    match method {
        SamplingMethod::Head { n }
        | SamplingMethod::Tail { n }
        | SamplingMethod::RandomCount { n } => (*n).min(total),
        SamplingMethod::RandomPercentage { percent }
        | SamplingMethod::HashDeterministic { percent, .. } => {
            (total as f64 * percent / 100.0).round() as u64
        }
        SamplingMethod::Systematic { step, offset } => {
            let step = (*step).max(1);
            let offset = offset.unwrap_or(0);
            if total > offset {
                (total - 1 - offset) / step + 1
            } else {
                0
            }
        }
        // Stratified/balanced projections need per-stratum sizes; the exact
        // pass fills them, and the projection mirrors it.
        SamplingMethod::Stratified { .. } | SamplingMethod::Balanced { .. } => 0,
    }
}

/// Build a non-binding preview: resolve the seed, size the scope, run the
/// deterministic selection to get exact counts, and project the formula counts.
pub fn preview(
    source: &dyn TabularSource,
    universe: &Universe,
    request: &SampleRequest,
    expected_revision: u64,
    ctx: Option<&JobCtx>,
) -> AppResult<SamplePreview> {
    let seed = resolve_seed(request.seed)?;
    let total = universe.len();

    let (plan, projections) = match &request.plan {
        SamplePlan::Sampling(method) => {
            let plan = plan_sample(source, universe, method, request.order, seed, ctx)?;
            let projected = match method {
                // Stratified/balanced: projection == exact (allocation is exact).
                SamplingMethod::Stratified { .. } | SamplingMethod::Balanced { .. } => {
                    plan.outputs[0].indices.len() as u64
                }
                other => project_sample(other, total),
            };
            let projections = vec![OutputProjection {
                name: plan.outputs[0].name.clone(),
                projected,
                exact: plan.outputs[0].indices.len() as u64,
            }];
            (plan, projections)
        }
        SamplePlan::Partitioning(spec) => {
            let plan = plan_partition(source, universe, spec, request.order, seed, ctx)?;
            let weight_sum: f64 = spec.parts.iter().map(|p| p.weight).sum();
            let projections = plan
                .outputs
                .iter()
                .zip(spec.parts.iter())
                .map(|(out, part)| OutputProjection {
                    name: out.name.clone(),
                    projected: (total as f64 * part.weight / weight_sum).round() as u64,
                    exact: out.indices.len() as u64,
                })
                .collect();
            (plan, projections)
        }
    };

    Ok(SamplePreview {
        seed,
        source_fingerprint: render_fingerprint(source.fingerprint()),
        scope: request.scope,
        order: request.order,
        total_rows: total,
        outputs: projections,
        strata: plan.strata,
        warnings: plan.warnings,
        expected_revision,
    })
}

// =====================================================================================
// Execution: materialise outputs to derived documents or CSV files
// =====================================================================================

/// SHA-256 over the canonical, boundary-safe row content stream of one output —
/// a content identity for derived-document outputs (byte-identical inputs hash
/// identically), independent of any file format.
struct ContentHasher {
    hasher: Sha256,
}

impl ContentHasher {
    fn new() -> ContentHasher {
        ContentHasher {
            hasher: Sha256::new(),
        }
    }

    fn update(&mut self, row: &TabularRow) {
        self.hasher.update(row_content_hash(row));
    }

    fn finish(self) -> String {
        format!("{:x}", self.hasher.finalize())
    }
}

/// Narrow a source row (`Option<String>` cells) to the derived builder's plain
/// `Vec<String>`, matching the CSV sink's missing→empty narrowing.
fn narrow_row(row: &TabularRow) -> Vec<String> {
    row.iter().map(|c| c.clone().unwrap_or_default()).collect()
}

/// Build the derived documents for every output plan. Documents are returned
/// UNREGISTERED; the caller registers them only once the whole job succeeds, so
/// a cancellation leaves no partial document behind. `doc_ids` supplies one id
/// per output, in order.
#[allow(clippy::too_many_arguments)]
pub fn execute_to_derived(
    source: &dyn TabularSource,
    plans: &[OutputPlan],
    doc_ids: &[u64],
    cache_root: PathBuf,
    seed: u64,
    scope: SampleScope,
    order: SampleOrder,
    total_rows: u64,
    method_label: &str,
    ctx: &JobCtx,
) -> AppResult<(Vec<Document>, SampleManifest)> {
    if plans.len() != doc_ids.len() {
        return Err(AppError::Other(
            "internal error: derived output/id count mismatch".to_string(),
        ));
    }
    let columns = source.columns();
    let headers: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
    ctx.set_total(plans.iter().map(|p| p.indices.len() as u64).sum());

    let mut docs = Vec::with_capacity(plans.len());
    let mut manifest_outputs = Vec::with_capacity(plans.len());
    for (i, plan) in plans.iter().enumerate() {
        ctx.set_part((i + 1) as u32);
        ctx.set_message(format!("building '{}'", plan.name));
        ctx.flush_progress();
        let mut builder = DerivedDocumentBuilder::new(
            headers.clone(),
            cache_root.clone(),
            crate::derived::SPILL_BUDGET,
        );
        let mut hasher = ContentHasher::new();
        read_indices(source, &plan.indices, Some(ctx), &mut |_abs, row| {
            hasher.update(row);
            builder.push_row(narrow_row(row))?;
            ctx.advance(1)?;
            Ok(true)
        })?;
        let doc = builder.finish(doc_ids[i], &mut |_| ctx.check())?;
        docs.push(doc);
        manifest_outputs.push(SampleManifestOutput {
            name: plan.name.clone(),
            file_name: None,
            rows: plan.indices.len() as u64,
            sha256: hasher.finish(),
        });
    }

    let manifest = SampleManifest {
        method: method_label.to_string(),
        seed,
        source_fingerprint: render_fingerprint(source.fingerprint()),
        scope,
        order,
        total_rows,
        outputs: manifest_outputs,
    };
    Ok((docs, manifest))
}

/// `<dir>/<base>-<label>.csv`, sanitising the label like the F04 split export.
fn output_path(dir: &Path, base: &str, label: &str, single: bool) -> PathBuf {
    if single {
        dir.join(format!("{base}.csv"))
    } else {
        let safe = crate::export_scope::sanitize_filename_part(label);
        dir.join(format!("{base}-{safe}.csv"))
    }
}

/// Write every output plan to a CSV file through the atomic streaming sink,
/// hash each file, and (optionally) write a JSON manifest. On ANY failure or
/// cancellation, every file already committed by this run is removed, so a
/// cancelled export never leaves incomplete outputs behind.
#[allow(clippy::too_many_arguments)]
pub fn execute_to_export(
    source: &dyn TabularSource,
    plans: &[OutputPlan],
    dir: &Path,
    base_name: &str,
    options: &ExportOptions,
    write_manifest: bool,
    seed: u64,
    scope: SampleScope,
    order: SampleOrder,
    total_rows: u64,
    method_label: &str,
    ctx: &JobCtx,
) -> AppResult<SampleManifest> {
    ctx.set_total(plans.iter().map(|p| p.indices.len() as u64).sum());
    let single = plans.len() == 1;
    let mut committed: Vec<PathBuf> = Vec::with_capacity(plans.len());

    let result = (|| -> AppResult<SampleManifest> {
        let mut manifest_outputs = Vec::with_capacity(plans.len());
        for (i, plan) in plans.iter().enumerate() {
            ctx.set_part((i + 1) as u32);
            ctx.set_message(format!("writing '{}'", plan.name));
            ctx.flush_progress();
            let path = output_path(dir, base_name, &plan.name, single);

            let mut sink = CsvSink::create(&path, options)?;
            sink.begin(&source.columns(), source.has_header_row())?;
            read_indices(source, &plan.indices, Some(ctx), &mut |_abs, row| {
                sink.write_rows(std::slice::from_ref(row), Some(ctx))?;
                ctx.advance(1)?;
                Ok(true)
            })?;
            sink.finish()?;
            committed.push(path.clone());

            manifest_outputs.push(SampleManifestOutput {
                name: plan.name.clone(),
                file_name: path.file_name().map(|n| n.to_string_lossy().to_string()),
                rows: plan.indices.len() as u64,
                sha256: sha256_file(&path)?,
            });
        }

        let manifest = SampleManifest {
            method: method_label.to_string(),
            seed,
            source_fingerprint: render_fingerprint(source.fingerprint()),
            scope,
            order,
            total_rows,
            outputs: manifest_outputs,
        };

        if write_manifest {
            let mpath = manifest_path(dir, base_name);
            let json = serde_json::to_vec_pretty(&manifest)
                .map_err(|e| AppError::Other(format!("manifest serialization failed: {e}")))?;
            crate::save::atomic_write(&mpath, BackupPolicy::None, |file| {
                use std::io::Write;
                file.write_all(&json)?;
                Ok(json.len() as u64)
            })?;
            committed.push(mpath);
        }

        Ok(manifest)
    })();

    if result.is_err() {
        // Remove every file this run committed (the in-flight file's own
        // staging is already cleaned by the sink's drop).
        for path in &committed {
            let _ = std::fs::remove_file(path);
        }
    }
    result
}

/// `<dir>/<base>.sample-manifest.json`.
fn manifest_path(dir: &Path, base: &str) -> PathBuf {
    dir.join(format!("{base}.sample-manifest.json"))
}

/// SHA-256 of a file's bytes, streamed so a large output is never fully
/// buffered.
fn sha256_file(path: &Path) -> AppResult<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

// =====================================================================================
// Document glue: build the universe for a scope
// =====================================================================================

/// Build the scope universe for a document: all rows, or the active filter
/// view (falling back to all rows when no filter is set).
pub fn universe_for(doc: &Document, scope: SampleScope) -> Universe {
    match scope {
        SampleScope::All => Universe::All(doc.n_rows() as u64),
        SampleScope::VisibleRows => match doc.filter_view() {
            Some(view) => Universe::Indices(view.iter().map(|&i| i as u64).collect()),
            None => Universe::All(doc.n_rows() as u64),
        },
    }
}

/// Run the full plan against a document source (shared by preview and execute).
pub fn plan_for(
    source: &dyn TabularSource,
    universe: &Universe,
    request: &SampleRequest,
    seed: u64,
    ctx: Option<&JobCtx>,
) -> AppResult<PlanResult> {
    match &request.plan {
        SamplePlan::Sampling(method) => {
            plan_sample(source, universe, method, request.order, seed, ctx)
        }
        SamplePlan::Partitioning(spec) => {
            plan_partition(source, universe, spec, request.order, seed, ctx)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobRegistry;
    use crate::parse::{parse, ParseSettings};
    use crate::tabular::{DocumentSource, MemSource};

    // ----- PRNG -------------------------------------------------------------

    #[test]
    fn splitmix64_matches_canonical_reference() {
        // Canonical SplitMix64 output for seed 0 (Vigna's reference).
        let mut sm = SplitMix64::new(0);
        assert_eq!(sm.next_u64(), 0xE220_A839_7B1D_CDAF);
        assert_eq!(sm.next_u64(), 0x6E78_9E6A_A1B9_65F4);
        assert_eq!(sm.next_u64(), 0x06C4_5D18_8009_454F);
    }

    #[test]
    fn xoshiro_is_a_pinned_pure_function_of_the_seed() {
        // Known-answer vector captured from the reference implementation.
        let mut p = Prng::seed(42);
        assert_eq!(p.next_u64(), 0x1578_0B2E_0C2E_C716);
        assert_eq!(p.next_u64(), 0x6104_D986_6D11_3A7E);
        assert_eq!(p.next_u64(), 0xAE17_5332_39E4_99A1);
        assert_eq!(p.next_u64(), 0xECB8_AD47_03B3_60A1);
        // Determinism: a fresh generator on the same seed replays exactly.
        let mut q = Prng::seed(42);
        assert_eq!(q.next_u64(), 0x1578_0B2E_0C2E_C716);
    }

    #[test]
    fn below_is_bounded_and_derive_streams_are_independent() {
        let mut p = Prng::seed(7);
        for _ in 0..10_000 {
            assert!(p.below(100) < 100);
        }
        assert_eq!(p.below(1), 0, "a bound of 1 always yields 0");
        // Different salts => different streams from the same seed.
        let a: Vec<u64> = (0..8).map(|_| Prng::derive(1, 1).next_u64()).collect();
        let b: Vec<u64> = (0..8).map(|_| Prng::derive(1, 2).next_u64()).collect();
        assert_ne!(a, b);
    }

    // ----- helpers ----------------------------------------------------------

    fn col(id: &str) -> TabularColumn {
        TabularColumn {
            name: id.to_uppercase(),
            id: Some(id.to_string()),
            schema: None,
        }
    }

    /// A synthetic streaming source that GENERATES rows on demand and never
    /// stores them — used to prove reservoir/streaming stays bounded over a
    /// source too large to materialise.
    struct SyntheticSource {
        n: u64,
    }

    impl TabularSource for SyntheticSource {
        fn columns(&self) -> Vec<TabularColumn> {
            vec![col("id"), col("bucket")]
        }
        fn row_count(&self) -> crate::tabular::RowCountHint {
            crate::tabular::RowCountHint::Exact(self.n)
        }
        fn read_rows(
            &self,
            offset: u64,
            limit: usize,
            ctx: Option<&JobCtx>,
        ) -> AppResult<Vec<TabularRow>> {
            if let Some(ctx) = ctx {
                ctx.check()?;
            }
            let start = offset.min(self.n);
            let end = start.saturating_add(limit as u64).min(self.n);
            Ok((start..end)
                .map(|i| vec![Some(i.to_string()), Some((i % 4).to_string())])
                .collect())
        }
        fn fingerprint(&self) -> ContentFingerprint {
            ContentFingerprint::Unknown
        }
    }

    /// A synthetic source whose single column `k` takes a DISTINCT value on
    /// every row, so grouping by it yields one stratum per row — used to prove
    /// the cardinality cap fires without materialising the source.
    struct DistinctKeySource {
        n: u64,
    }

    impl TabularSource for DistinctKeySource {
        fn columns(&self) -> Vec<TabularColumn> {
            vec![col("k")]
        }
        fn row_count(&self) -> crate::tabular::RowCountHint {
            crate::tabular::RowCountHint::Exact(self.n)
        }
        fn read_rows(
            &self,
            offset: u64,
            limit: usize,
            ctx: Option<&JobCtx>,
        ) -> AppResult<Vec<TabularRow>> {
            if let Some(ctx) = ctx {
                ctx.check()?;
            }
            let start = offset.min(self.n);
            let end = start.saturating_add(limit as u64).min(self.n);
            Ok((start..end).map(|i| vec![Some(i.to_string())]).collect())
        }
        fn fingerprint(&self) -> ContentFingerprint {
            ContentFingerprint::Unknown
        }
    }

    fn mem(rows: usize, cols: usize) -> MemSource {
        let columns: Vec<TabularColumn> = (0..cols).map(|c| col(&format!("c{c}"))).collect();
        let data: Vec<TabularRow> = (0..rows)
            .map(|r| {
                (0..cols)
                    .map(|c| Some(format!("r{r}c{c}")))
                    .collect::<TabularRow>()
            })
            .collect();
        MemSource::new(columns, data)
    }

    fn ctx() -> (JobRegistry, JobCtx) {
        let registry = JobRegistry::default();
        let c = registry.begin("sampling", None, |_| {});
        (registry, c)
    }

    fn plan_sampling(source: &dyn TabularSource, n: u64, method: SamplingMethod) -> Vec<u64> {
        let u = Universe::All(n);
        plan_sample(source, &u, &method, SampleOrder::SourceOrder, 99, None)
            .unwrap()
            .outputs[0]
            .indices
            .clone()
    }

    // ----- head / tail / systematic ----------------------------------------

    #[test]
    fn head_tail_and_systematic_select_the_right_rows() {
        let s = mem(10, 1);
        assert_eq!(
            plan_sampling(&s, 10, SamplingMethod::Head { n: 3 }),
            vec![0, 1, 2]
        );
        assert_eq!(
            plan_sampling(&s, 10, SamplingMethod::Tail { n: 3 }),
            vec![7, 8, 9]
        );
        // Over-large n clamps.
        assert_eq!(
            plan_sampling(&s, 10, SamplingMethod::Head { n: 99 }).len(),
            10
        );
        // Every 3rd row from offset 1: 1, 4, 7.
        assert_eq!(
            plan_sampling(
                &s,
                10,
                SamplingMethod::Systematic {
                    step: 3,
                    offset: Some(1)
                }
            ),
            vec![1, 4, 7]
        );
    }

    // ----- determinism ------------------------------------------------------

    #[test]
    fn same_seed_identical_different_seed_differs() {
        let s = mem(1000, 2);
        let u = Universe::All(1000);
        let method = SamplingMethod::RandomCount { n: 100 };
        let a = plan_sample(&s, &u, &method, SampleOrder::SourceOrder, 123, None).unwrap();
        let b = plan_sample(&s, &u, &method, SampleOrder::SourceOrder, 123, None).unwrap();
        let c = plan_sample(&s, &u, &method, SampleOrder::SourceOrder, 124, None).unwrap();
        assert_eq!(
            a.outputs[0].indices, b.outputs[0].indices,
            "same seed → identical"
        );
        assert_ne!(
            a.outputs[0].indices, c.outputs[0].indices,
            "different seed → different selection"
        );
        assert_eq!(a.outputs[0].indices.len(), 100);
    }

    #[test]
    fn export_bytes_are_byte_identical_across_runs() {
        let s = mem(200, 3);
        let u = Universe::All(200);
        let method = SamplingMethod::RandomPercentage { percent: 40.0 };
        let plan = plan_sample(&s, &u, &method, SampleOrder::SourceOrder, 5, None).unwrap();

        let opts = export_opts();
        let run = |dir: &Path| {
            let (_r, c) = ctx();
            execute_to_export(
                &s,
                &plan.outputs,
                dir,
                "sample",
                &opts,
                true,
                5,
                SampleScope::All,
                SampleOrder::SourceOrder,
                200,
                "randomPercentage",
                &c,
            )
            .unwrap();
            std::fs::read(dir.join("sample.csv")).unwrap()
        };
        let d1 = tempfile::tempdir().unwrap();
        let d2 = tempfile::tempdir().unwrap();
        assert_eq!(
            run(d1.path()),
            run(d2.path()),
            "same seed → byte-identical output"
        );
    }

    fn export_opts() -> ExportOptions {
        ExportOptions {
            delimiter: ",".into(),
            encoding: "UTF-8".into(),
            quote_style: "minimal".into(),
            line_ending: "lf".into(),
            bom: false,
            include_headers: true,
            backup: BackupPolicy::None,
        }
    }

    // ----- order preservation ----------------------------------------------

    #[test]
    fn source_order_is_ascending_shuffle_reorders_same_set() {
        let s = mem(500, 1);
        let u = Universe::All(500);
        let method = SamplingMethod::RandomCount { n: 50 };
        let ordered = plan_sample(&s, &u, &method, SampleOrder::SourceOrder, 8, None)
            .unwrap()
            .outputs[0]
            .indices
            .clone();
        let shuffled = plan_sample(&s, &u, &method, SampleOrder::Shuffle, 8, None)
            .unwrap()
            .outputs[0]
            .indices
            .clone();
        assert!(
            ordered.windows(2).all(|w| w[0] < w[1]),
            "source order is strictly ascending"
        );
        let mut a = ordered.clone();
        let mut b = shuffled.clone();
        a.sort_unstable();
        b.sort_unstable();
        assert_eq!(a, b, "shuffle keeps the same selected set");
        assert_ne!(ordered, shuffled, "shuffle changes the emission order");
    }

    // ----- reservoir bound --------------------------------------------------

    #[test]
    fn reservoir_stays_bounded_over_a_huge_source() {
        // A million rows generated on demand; the reservoir must never hold
        // more than `n` and its backing allocation must not grow past `n`.
        let source = SyntheticSource { n: 1_000_000 };
        let u = Universe::All(source.n);

        let cap = 100usize;
        let mut reservoir: Reservoir<u64> = Reservoir::new(cap, Prng::derive(7, salt::RESERVOIR));
        scan_universe(&source, &u, None, |_pos, abs, _row| {
            reservoir.add(abs);
            assert!(reservoir.len() <= cap, "reservoir never exceeds capacity");
            assert!(
                reservoir.allocated() <= cap,
                "reservoir allocation never grows past capacity"
            );
            Ok(true)
        })
        .unwrap();
        assert_eq!(reservoir.len(), cap);
        assert_eq!(reservoir.seen, 1_000_000);

        // The full sampling path yields exactly `cap` in-range indices.
        let out = plan_sampling(
            &source,
            source.n,
            SamplingMethod::RandomCount { n: cap as u64 },
        );
        assert_eq!(out.len(), cap);
        assert!(out.iter().all(|&i| i < 1_000_000));
        let unique: std::collections::HashSet<u64> = out.iter().copied().collect();
        assert_eq!(unique.len(), cap, "no row selected twice");
    }

    // ----- strata cardinality cap ------------------------------------------

    #[test]
    fn high_cardinality_stratify_key_is_capped() {
        // One distinct stratum per row, just past the cap: grouping must fail
        // fast (bounded memory + bounded IPC strata payload) instead of building
        // an unbounded map, like groupby's MAX_GROUPS / cluster's guard.
        let source = DistinctKeySource {
            n: MAX_STRATA as u64 + 1,
        };
        let u = Universe::All(source.n);
        let method = SamplingMethod::Balanced {
            columns: vec!["k".into()],
            per_stratum: 1,
        };
        let err = plan_sample(&source, &u, &method, SampleOrder::SourceOrder, 1, None).unwrap_err();
        assert!(
            matches!(err, AppError::InvalidArg(_)),
            "expected an invalid-argument error, got {err:?}"
        );
        assert!(
            err.to_string().contains("strata") || err.to_string().contains("group"),
            "message should name strata/groups: {err}"
        );

        // Comfortably under the cap still groups fine (one row per stratum).
        let ok_source = DistinctKeySource { n: 10 };
        let ok_u = Universe::All(ok_source.n);
        let plan = plan_sample(
            &ok_source,
            &ok_u,
            &method,
            SampleOrder::SourceOrder,
            1,
            None,
        )
        .unwrap();
        assert_eq!(plan.outputs[0].indices.len(), 10);
        assert_eq!(plan.strata.as_ref().unwrap().len(), 10);
    }

    #[test]
    fn high_cardinality_group_partition_key_is_capped() {
        // The same guard protects group-preserving partitioning.
        let source = DistinctKeySource {
            n: MAX_STRATA as u64 + 1,
        };
        let u = Universe::All(source.n);
        let spec = PartitionSpec {
            parts: vec![
                PartitionOutput {
                    name: "a".into(),
                    weight: 0.5,
                },
                PartitionOutput {
                    name: "b".into(),
                    weight: 0.5,
                },
            ],
            stratify_by: vec![],
            group_by: vec!["k".into()],
            allow_overlap: false,
        };
        let err =
            plan_partition(&source, &u, &spec, SampleOrder::SourceOrder, 1, None).unwrap_err();
        assert!(
            matches!(err, AppError::InvalidArg(_)),
            "expected an invalid-argument error, got {err:?}"
        );
    }

    // ----- hash-based -------------------------------------------------------

    #[test]
    fn hash_based_is_stable_when_unrelated_rows_change() {
        // The kept subset is a function of each row's own content + the seed, so
        // adding rows never changes whether an existing row is kept.
        let small = mem(100, 2);
        let large = mem(200, 2);
        let method = SamplingMethod::HashDeterministic {
            columns: None,
            percent: 50.0,
        };
        let a = plan_sampling(&small, 100, method.clone());
        let b = plan_sampling(&large, 200, method);
        // Every row 0..100 that `a` kept is also kept by `b` (same content).
        let b_set: std::collections::HashSet<u64> = b.iter().copied().collect();
        for &i in &a {
            assert!(b_set.contains(&i), "row {i} kept in small but not large");
        }
        assert!(!a.is_empty() && a.len() < 100, "keeps roughly half");
    }

    // ----- stratified -------------------------------------------------------

    fn strat_source() -> MemSource {
        // Column c0 is the stratum key with 3 values in a 6:3:1 ratio.
        let mut rows: Vec<TabularRow> = Vec::new();
        for _ in 0..600 {
            rows.push(vec![Some("A".into()), Some("x".into())]);
        }
        for _ in 0..300 {
            rows.push(vec![Some("B".into()), Some("y".into())]);
        }
        for _ in 0..100 {
            rows.push(vec![Some("C".into()), Some("z".into())]);
        }
        MemSource::new(vec![col("c0"), col("c1")], rows)
    }

    #[test]
    fn stratified_is_proportional_within_tolerance() {
        let s = strat_source();
        let u = Universe::All(1000);
        let method = SamplingMethod::Stratified {
            columns: vec!["c0".into()],
            fraction: 0.1,
            tolerance: 0.001,
        };
        let plan = plan_sample(&s, &u, &method, SampleOrder::SourceOrder, 3, None).unwrap();
        assert_eq!(plan.outputs[0].indices.len(), 100, "10% of 1000");
        assert!(plan.warnings.is_empty(), "within tolerance → no warning");

        let strata = plan.strata.unwrap();
        // Each stratum keeps ~10% (A=60, B=30, C=10).
        let by_key: HashMap<String, u64> = strata
            .iter()
            .map(|r| (r.key[0].clone(), r.selected))
            .collect();
        assert_eq!(by_key["A"], 60);
        assert_eq!(by_key["B"], 30);
        assert_eq!(by_key["C"], 10);
    }

    #[test]
    fn balanced_reports_shortfall() {
        let s = strat_source();
        let u = Universe::All(1000);
        let method = SamplingMethod::Balanced {
            columns: vec!["c0".into()],
            per_stratum: 200,
        };
        let plan = plan_sample(&s, &u, &method, SampleOrder::SourceOrder, 1, None).unwrap();
        // A:200, B:200, C:100 (only 100 available → shortfall of 100).
        assert_eq!(plan.outputs[0].indices.len(), 500);
        assert!(
            plan.warnings.iter().any(|w| w.contains("short")),
            "shortfall warning for stratum C"
        );
    }

    // ----- partitioning -----------------------------------------------------

    fn parts() -> PartitionSpec {
        PartitionSpec {
            parts: vec![
                PartitionOutput {
                    name: "train".into(),
                    weight: 0.7,
                },
                PartitionOutput {
                    name: "val".into(),
                    weight: 0.15,
                },
                PartitionOutput {
                    name: "test".into(),
                    weight: 0.15,
                },
            ],
            stratify_by: vec![],
            group_by: vec![],
            allow_overlap: false,
        }
    }

    #[test]
    fn weighted_partitions_are_disjoint_exact_and_cover_everything() {
        let s = mem(1000, 1);
        let u = Universe::All(1000);
        let spec = parts();
        let plan = plan_partition(&s, &u, &spec, SampleOrder::SourceOrder, 11, None).unwrap();
        assert_eq!(plan.outputs.len(), 3);
        // Exact largest-remainder counts summing to the total.
        let counts: Vec<usize> = plan.outputs.iter().map(|o| o.indices.len()).collect();
        assert_eq!(counts, vec![700, 150, 150]);

        // Disjoint AND covering: every row appears exactly once.
        let mut all: Vec<u64> = plan
            .outputs
            .iter()
            .flat_map(|o| o.indices.clone())
            .collect();
        all.sort_unstable();
        assert_eq!(all, (0..1000).collect::<Vec<u64>>(), "no repeats, no gaps");
    }

    #[test]
    fn disjointness_holds_across_seeds_property() {
        let s = mem(777, 1);
        let u = Universe::All(777);
        let spec = parts();
        for seed in 0..25u64 {
            let plan = plan_partition(&s, &u, &spec, SampleOrder::SourceOrder, seed, None).unwrap();
            let mut seen = std::collections::HashSet::new();
            for out in &plan.outputs {
                for &i in &out.indices {
                    assert!(
                        seen.insert(i),
                        "row {i} appeared in two partitions (seed {seed})"
                    );
                }
            }
            assert_eq!(seen.len(), 777, "all rows covered (seed {seed})");
        }
    }

    #[test]
    fn group_preserving_never_splits_a_group() {
        // 100 groups of 10 rows each; group key in column c0.
        let mut rows: Vec<TabularRow> = Vec::new();
        for g in 0..100 {
            for _ in 0..10 {
                rows.push(vec![Some(format!("g{g}")), Some("v".into())]);
            }
        }
        let s = MemSource::new(vec![col("c0"), col("c1")], rows);
        let u = Universe::All(1000);
        let spec = PartitionSpec {
            parts: vec![
                PartitionOutput {
                    name: "a".into(),
                    weight: 0.5,
                },
                PartitionOutput {
                    name: "b".into(),
                    weight: 0.5,
                },
            ],
            stratify_by: vec![],
            group_by: vec!["c0".into()],
            allow_overlap: false,
        };
        let plan = plan_partition(&s, &u, &spec, SampleOrder::SourceOrder, 4, None).unwrap();

        // Map each row's group to its partition; every group must be single-valued.
        let mut group_partition: HashMap<u64, usize> = HashMap::new();
        for (p, out) in plan.outputs.iter().enumerate() {
            for &row in &out.indices {
                let group = row / 10; // 10 rows per group, contiguous
                match group_partition.get(&group) {
                    Some(&existing) => {
                        assert_eq!(existing, p, "group {group} split across partitions")
                    }
                    None => {
                        group_partition.insert(group, p);
                    }
                }
            }
        }
        assert_eq!(group_partition.len(), 100, "every group placed");
        // Disjoint + covering still holds.
        let mut all: Vec<u64> = plan
            .outputs
            .iter()
            .flat_map(|o| o.indices.clone())
            .collect();
        all.sort_unstable();
        assert_eq!(all, (0..1000).collect::<Vec<u64>>());
    }

    #[test]
    fn stratified_partition_splits_each_stratum() {
        let s = strat_source(); // A:600 B:300 C:100
        let u = Universe::All(1000);
        let spec = PartitionSpec {
            parts: vec![
                PartitionOutput {
                    name: "train".into(),
                    weight: 0.8,
                },
                PartitionOutput {
                    name: "test".into(),
                    weight: 0.2,
                },
            ],
            stratify_by: vec!["c0".into()],
            group_by: vec![],
            allow_overlap: false,
        };
        let plan = plan_partition(&s, &u, &spec, SampleOrder::SourceOrder, 2, None).unwrap();
        // Each stratum split 80/20 exactly: train = 480+240+80 = 800.
        assert_eq!(plan.outputs[0].indices.len(), 800);
        assert_eq!(plan.outputs[1].indices.len(), 200);
        let mut all: Vec<u64> = plan
            .outputs
            .iter()
            .flat_map(|o| o.indices.clone())
            .collect();
        all.sort_unstable();
        assert_eq!(all, (0..1000).collect::<Vec<u64>>());
    }

    #[test]
    fn partition_validation_rejects_bad_specs() {
        let s = mem(10, 1);
        let u = Universe::All(10);
        let bad = |spec: PartitionSpec| {
            plan_partition(&s, &u, &spec, SampleOrder::SourceOrder, 1, None).is_err()
        };
        // Fewer than two partitions.
        assert!(bad(PartitionSpec {
            parts: vec![PartitionOutput {
                name: "only".into(),
                weight: 1.0
            }],
            stratify_by: vec![],
            group_by: vec![],
            allow_overlap: false,
        }));
        // Overlap requested (unsupported).
        assert!(bad(PartitionSpec {
            allow_overlap: true,
            ..parts()
        }));
        // Duplicate names.
        assert!(bad(PartitionSpec {
            parts: vec![
                PartitionOutput {
                    name: "x".into(),
                    weight: 1.0
                },
                PartitionOutput {
                    name: "x".into(),
                    weight: 1.0
                },
            ],
            stratify_by: vec![],
            group_by: vec![],
            allow_overlap: false,
        }));
        // Group + stratify combined.
        assert!(bad(PartitionSpec {
            stratify_by: vec!["c0".into()],
            group_by: vec!["c0".into()],
            ..parts()
        }));
    }

    // ----- scope ------------------------------------------------------------

    #[test]
    fn visible_scope_only_draws_from_the_filter_view() {
        let s = mem(10, 1);
        // Visible rows are the odd indices.
        let u = Universe::Indices(vec![1, 3, 5, 7, 9]);
        let out = plan_sample(
            &s,
            &u,
            &SamplingMethod::Head { n: 3 },
            SampleOrder::SourceOrder,
            1,
            None,
        )
        .unwrap();
        assert_eq!(out.outputs[0].indices, vec![1, 3, 5], "first 3 of the view");
    }

    // ----- preview ----------------------------------------------------------

    #[test]
    fn preview_reports_projected_and_exact_and_resolves_seed() {
        let s = mem(1000, 1);
        let u = Universe::All(1000);
        let request = SampleRequest {
            plan: SamplePlan::Sampling(SamplingMethod::RandomPercentage { percent: 30.0 }),
            scope: SampleScope::All,
            order: SampleOrder::SourceOrder,
            seed: Some(77),
            destination: SampleDestination::DerivedDocuments,
        };
        let p = preview(&s, &u, &request, 0, None).unwrap();
        assert_eq!(p.seed, 77);
        assert_eq!(p.total_rows, 1000);
        assert_eq!(p.outputs[0].projected, 300, "formula projects 30%");
        // Bernoulli's realised count is near but not exactly 300.
        assert!((p.outputs[0].exact as i64 - 300).abs() < 60);
        assert!(p.strata.is_none());

        // A None seed resolves to a concrete value.
        let mut req2 = request.clone();
        req2.seed = None;
        let p2 = preview(&s, &u, &req2, 0, None).unwrap();
        // Overwhelmingly unlikely to be exactly 0; just assert it ran.
        assert_eq!(p2.total_rows, 1000);
    }

    // ----- execution: derived documents ------------------------------------

    #[test]
    fn derived_outputs_round_trip_and_hash_is_deterministic() {
        let csv = {
            let mut s = String::from("id,name\n");
            for i in 0..50 {
                s.push_str(&format!("{i},name-{i}\n"));
            }
            s
        };
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        let doc = Document::from_parsed(1, None, parsed, true);
        let src = DocumentSource::new(&doc);
        let u = Universe::All(50);
        let spec = parts();
        let plan = plan_partition(&src, &u, &spec, SampleOrder::SourceOrder, 9, None).unwrap();

        let dir = tempfile::tempdir().unwrap();
        let ids = vec![10u64, 11, 12];
        let (_r, c) = ctx();
        let (docs, manifest) = execute_to_derived(
            &src,
            &plan.outputs,
            &ids,
            dir.path().to_path_buf(),
            9,
            SampleScope::All,
            SampleOrder::SourceOrder,
            50,
            "partition",
            &c,
        )
        .unwrap();
        assert_eq!(docs.len(), 3);
        let total: usize = docs.iter().map(|d| d.n_rows()).sum();
        assert_eq!(total, 50, "outputs cover every source row exactly once");
        assert_eq!(docs[0].headers(), &["id", "name"]);

        // Same seed → identical content hashes.
        let (_r2, c2) = ctx();
        let (_docs2, manifest2) = execute_to_derived(
            &src,
            &plan.outputs,
            &ids,
            dir.path().to_path_buf(),
            9,
            SampleScope::All,
            SampleOrder::SourceOrder,
            50,
            "partition",
            &c2,
        )
        .unwrap();
        let h1: Vec<&String> = manifest.outputs.iter().map(|o| &o.sha256).collect();
        let h2: Vec<&String> = manifest2.outputs.iter().map(|o| &o.sha256).collect();
        assert_eq!(h1, h2, "content hashes are deterministic");
    }

    // ----- execution: exports + manifest -----------------------------------

    #[test]
    fn export_manifest_hashes_match_the_files_on_disk() {
        let s = mem(60, 2);
        let u = Universe::All(60);
        let spec = parts();
        let plan = plan_partition(&s, &u, &spec, SampleOrder::SourceOrder, 6, None).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (_r, c) = ctx();
        let manifest = execute_to_export(
            &s,
            &plan.outputs,
            dir.path(),
            "split",
            &export_opts(),
            true,
            6,
            SampleScope::All,
            SampleOrder::SourceOrder,
            60,
            "partition",
            &c,
        )
        .unwrap();

        assert_eq!(manifest.outputs.len(), 3);
        for out in &manifest.outputs {
            let name = out.file_name.as_ref().unwrap();
            let bytes = std::fs::read(dir.path().join(name)).unwrap();
            let expected = format!("{:x}", Sha256::digest(&bytes));
            assert_eq!(&out.sha256, &expected, "{name}");
            // header + rows.
            let lines = String::from_utf8(bytes).unwrap().lines().count();
            assert_eq!(lines as u64, out.rows + 1);
        }

        // The manifest file exists and round-trips.
        let mpath = dir.path().join("split.sample-manifest.json");
        let json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&mpath).unwrap()).unwrap();
        assert_eq!(json["seed"], 6);
        assert_eq!(json["method"], "partition");
        assert_eq!(json["outputs"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn single_output_export_keeps_the_base_name() {
        let s = mem(20, 1);
        let u = Universe::All(20);
        let plan = plan_sample(
            &s,
            &u,
            &SamplingMethod::Head { n: 5 },
            SampleOrder::SourceOrder,
            1,
            None,
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (_r, c) = ctx();
        execute_to_export(
            &s,
            &plan.outputs,
            dir.path(),
            "head",
            &export_opts(),
            false,
            1,
            SampleScope::All,
            SampleOrder::SourceOrder,
            20,
            "head",
            &c,
        )
        .unwrap();
        assert!(
            dir.path().join("head.csv").exists(),
            "single output uses the base name"
        );
    }

    // ----- cancellation cleanup --------------------------------------------

    #[test]
    fn cancelled_export_removes_all_committed_outputs() {
        let s = mem(100, 2);
        let u = Universe::All(100);
        let spec = parts();
        let plan = plan_partition(&s, &u, &spec, SampleOrder::SourceOrder, 3, None).unwrap();
        let dir = tempfile::tempdir().unwrap();

        let registry = JobRegistry::default();
        let c = registry.begin("sampling", None, |_| {});
        registry.cancel(c.id); // cancel before any write
        let result = execute_to_export(
            &s,
            &plan.outputs,
            dir.path(),
            "split",
            &export_opts(),
            true,
            3,
            SampleScope::All,
            SampleOrder::SourceOrder,
            100,
            "partition",
            &c,
        );
        assert!(matches!(result, Err(AppError::Cancelled)));
        let leftover = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(
            leftover, 0,
            "no committed files, no staging litter, no manifest"
        );
    }

    #[test]
    fn cancel_mid_run_deletes_earlier_partitions() {
        // A source large enough that cancellation lands after the first output
        // file is committed but before the run finishes.
        let s = mem(30_000, 2);
        let u = Universe::All(30_000);
        let spec = parts();
        let plan = plan_partition(&s, &u, &spec, SampleOrder::SourceOrder, 3, None).unwrap();
        let dir = tempfile::tempdir().unwrap();

        // A ctx that cancels itself once the second output starts.
        let registry = std::sync::Arc::new(JobRegistry::default());
        let reg2 = std::sync::Arc::clone(&registry);
        let c = registry.begin("sampling", None, move |event| {
            if let crate::job::JobEvent::Progress(p) = event {
                if p.part == Some(2) {
                    reg2.cancel(p.job_id);
                }
            }
        });
        let result = execute_to_export(
            &s,
            &plan.outputs,
            dir.path(),
            "split",
            &export_opts(),
            true,
            3,
            SampleScope::All,
            SampleOrder::SourceOrder,
            30_000,
            "partition",
            &c,
        );
        assert!(matches!(result, Err(AppError::Cancelled)));
        let leftover = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(
            leftover, 0,
            "the already-committed first partition was removed"
        );
    }
}
