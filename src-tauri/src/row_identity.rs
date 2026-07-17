//! Shared row-identity model, consumed by upcoming features: F40 row
//! annotations, F46 patches and F47 three-way merge.
//!
//! Three identity mechanisms, in order of strength:
//!
//! 1. **Editor row ids** ([`RowIds`]) — session-stable ids for the rows of an
//!    EDITABLE document. document.rs/journal.rs track stable COLUMN ids (F12)
//!    and per-op ids (F15/F16) but have no per-row id, so the allocator lives
//!    here; the first consumer (F40) wires it into the document's mutation
//!    paths by mirroring every row operation (insert/remove/move + the exact
//!    undo inverses).
//! 2. **Source record numbers** — indexed/read-only documents are immutable,
//!    so the 0-based data-record number IS a stable identity (any external
//!    rewrite of the backing file is already detected per read).
//! 3. **Composite keys** ([`KeySpec`]) — an ordered list of key columns,
//!    addressed by their STABLE column ids (F31), with configurable,
//!    deterministic normalization. Duplicate keys are reported explicitly —
//!    every involved row is flagged, never silently first-wins.
//!
//! [`row_content_hash`] complements all three with a content identity:
//! a SHA-256 over a cell-boundary-safe encoding, so `["ab","c"]`,
//! `["a","bc"]`, and missing-vs-empty cells all hash differently.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::error::{AppError, AppResult};
use crate::job::JobCtx;
use crate::tabular::{TabularRow, TabularSource, DEFAULT_WINDOW};

/// How one row is identified, across edits (editor ids), across reloads
/// (record numbers) or across documents (composite keys).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RowIdentity {
    /// Session-stable id of a row in an editable document ([`RowIds`]).
    EditorRow { row_uid: u64 },
    /// 0-based data-record number in an indexed/read-only source.
    SourceRecord { record: u64 },
    /// Normalized composite key built from declared key columns.
    Key { key: CompositeKey },
}

// ----- editor row ids --------------------------------------------------------------

/// Session-stable row ids for one editable document, maintained in lockstep
/// with its row storage: positional at creation, minted fresh on insert
/// (never reused), restored verbatim on undo. Lookup by id is a linear scan
/// — fine for the id→position queries F40 makes on user actions; bulk
/// consumers should walk positions instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowIds {
    ids: Vec<u64>,
    next: u64,
}

impl RowIds {
    /// Ids for a freshly loaded document: positional, `0..n_rows`.
    pub fn new(n_rows: usize) -> RowIds {
        RowIds {
            ids: (0..n_rows as u64).collect(),
            next: n_rows as u64,
        }
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// The id of the row currently at `row`.
    pub fn id_at(&self, row: usize) -> Option<u64> {
        self.ids.get(row).copied()
    }

    /// The current position of `id` (linear scan; see the type docs).
    pub fn position_of(&self, id: u64) -> Option<usize> {
        self.ids.iter().position(|&x| x == id)
    }

    /// Mirror a row insertion: mint `count` fresh ids at `at`. Returns the
    /// minted ids (record them in the op for undo/redo symmetry).
    pub fn insert(&mut self, at: usize, count: usize) -> AppResult<Vec<u64>> {
        if at > self.ids.len() {
            return Err(AppError::invalid("row index out of range"));
        }
        let minted: Vec<u64> = (self.next..self.next + count as u64).collect();
        self.next += count as u64;
        self.ids.splice(at..at, minted.iter().copied());
        Ok(minted)
    }

    /// Mirror a row deletion. `rows` in any order (duplicates ignored);
    /// returns the removed `(position, id)` pairs in ascending position
    /// order — exactly what [`RowIds::restore`] needs to undo the deletion.
    pub fn remove(&mut self, rows: &[usize]) -> AppResult<Vec<(usize, u64)>> {
        let mut sorted: Vec<usize> = rows.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        if let Some(&bad) = sorted.iter().find(|&&r| r >= self.ids.len()) {
            return Err(AppError::invalid(format!("row {bad} is out of range")));
        }
        let mut removed = Vec::with_capacity(sorted.len());
        for &r in sorted.iter().rev() {
            removed.push((r, self.ids.remove(r)));
        }
        removed.reverse();
        Ok(removed)
    }

    /// Undo a deletion: reinstate the recorded `(position, id)` pairs
    /// (ascending, as [`RowIds::remove`] returned them). Ids are restored
    /// verbatim — never re-minted — so identities survive undo/redo.
    pub fn restore(&mut self, entries: &[(usize, u64)]) -> AppResult<()> {
        for &(at, id) in entries {
            if at > self.ids.len() {
                return Err(AppError::invalid("restore position out of range"));
            }
            self.ids.insert(at, id);
        }
        Ok(())
    }

    /// Mirror a row move: the id at `from` ends up at `to`.
    pub fn move_row(&mut self, from: usize, to: usize) -> AppResult<()> {
        if from >= self.ids.len() || to >= self.ids.len() {
            return Err(AppError::invalid("row index out of range"));
        }
        let id = self.ids.remove(from);
        self.ids.insert(to, id);
        Ok(())
    }
}

// ----- composite keys --------------------------------------------------------------

/// Normalizations applied to key cells before comparison, in this fixed
/// order (deterministic by construction):
///
/// 1. **Unicode NFKC** — compatibility decomposition + canonical
///    composition (`ﬁ` → `fi`, fullwidth `１` → `1`, NBSP → space).
///    Applied FIRST so later steps see canonical text.
/// 2. **Case fold** — Unicode lowercasing (`str::to_lowercase`).
/// 3. **Trim** — strip leading/trailing `White_Space` characters. Applied
///    LAST so whitespace that NFKC canonicalized (e.g. NBSP) is caught.
///
/// Missing cells (`None`) never normalize into empty strings: missing and
/// empty stay distinct key values.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct KeyNormalization {
    pub trim: bool,
    pub case_fold: bool,
    pub unicode_nfkc: bool,
}

/// Which columns form the key: STABLE column ids (F12/F31), in key order,
/// plus the normalization to apply. Column ids — not positions or header
/// text — so specs survive renames and reorders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeySpec {
    pub columns: Vec<String>,
    #[serde(default)]
    pub normalization: KeyNormalization,
}

/// A normalized composite key. Cells stay separate (no string joining), so
/// boundaries are unambiguous by construction: `["ab","c"] != ["a","bc"]`,
/// and a missing key cell differs from an empty one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CompositeKey(pub Vec<Option<String>>);

/// Apply `n` to one cell value (see [`KeyNormalization`] for the order).
pub fn normalize_key_cell(value: &str, n: &KeyNormalization) -> String {
    let mut v: String = if n.unicode_nfkc {
        value.nfkc().collect()
    } else {
        value.to_string()
    };
    if n.case_fold {
        v = v.to_lowercase();
    }
    if n.trim {
        v = v.trim().to_string();
    }
    v
}

/// Build the composite key of one row. `positions` are the key columns'
/// current positions in the row (resolved from stable ids by the caller or
/// by [`build_key_index`]). A short row's absent cells are missing (`None`).
pub fn composite_key(row: &TabularRow, positions: &[usize], n: &KeyNormalization) -> CompositeKey {
    CompositeKey(
        positions
            .iter()
            .map(|&p| {
                row.get(p)
                    .and_then(|c| c.as_deref())
                    .map(|c| normalize_key_cell(c, n))
            })
            .collect(),
    )
}

// ----- row content hashing ---------------------------------------------------------

/// SHA-256 of a row under a cell-boundary-safe encoding: per cell, a
/// presence tag (0 = missing, 1 = present) and, when present, the cell's
/// byte length (u64 LE) followed by its bytes. No concatenation ambiguity:
/// `["ab","c"]`, `["a","bc"]`, `[None]` and `[Some("")]` all differ.
pub fn row_content_hash(row: &[Option<String>]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for cell in row {
        match cell {
            None => hasher.update([0u8]),
            Some(s) => {
                hasher.update([1u8]);
                hasher.update((s.len() as u64).to_le_bytes());
                hasher.update(s.as_bytes());
            }
        }
    }
    hasher.finalize().into()
}

/// Hex rendering of [`row_content_hash`], for manifests and patch files.
pub fn row_content_hash_hex(row: &[Option<String>]) -> String {
    row_content_hash(row)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ----- the resolver ----------------------------------------------------------------

/// One key that matched more than one row. EVERY involved row is listed;
/// consumers must treat all of them as ambiguous (never first-wins).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DuplicateKey {
    pub key: CompositeKey,
    /// Absolute row numbers, in source order.
    pub rows: Vec<u64>,
}

/// A key → rows index over one source, with explicit ambiguity reporting.
#[derive(Debug, Clone, Default)]
pub struct KeyIndex {
    /// Every key, mapped to ALL rows carrying it (source order).
    by_key: HashMap<CompositeKey, Vec<u64>>,
    /// Keys carried by more than one row, ordered by first occurrence.
    pub duplicates: Vec<DuplicateKey>,
    /// Rows read while building the index.
    pub rows_indexed: u64,
}

impl KeyIndex {
    /// All rows carrying `key` (empty when absent).
    pub fn rows_for(&self, key: &CompositeKey) -> &[u64] {
        self.by_key.get(key).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The row carrying `key`, when it is unambiguous. `Err` carries every
    /// involved row for a duplicated key; `Ok(None)` means absent.
    pub fn unique_row(&self, key: &CompositeKey) -> Result<Option<u64>, &[u64]> {
        match self.rows_for(key) {
            [] => Ok(None),
            [one] => Ok(Some(*one)),
            many => Err(many),
        }
    }

    pub fn has_duplicates(&self) -> bool {
        !self.duplicates.is_empty()
    }
}

/// Stream `source` and build its key index for `spec`. Key columns are
/// resolved by STABLE column id ([`crate::tabular::TabularColumn::id`])
/// against the source's schema; unknown ids fail up front. Resolution is
/// deliberately by id, never by header text, so a spec survives renames and
/// reorders — a non-document source must therefore expose stable ids for its
/// key columns (see [`crate::tabular::TabularColumn::id`] for the naming
/// convention). Duplicate keys land in [`KeyIndex::duplicates`] with every
/// involved row flagged.
pub fn build_key_index(
    source: &dyn TabularSource,
    spec: &KeySpec,
    ctx: Option<&JobCtx>,
) -> AppResult<KeyIndex> {
    if spec.columns.is_empty() {
        return Err(AppError::invalid("pick at least one key column"));
    }
    let cols = source.columns();
    let mut positions = Vec::with_capacity(spec.columns.len());
    for id in &spec.columns {
        let pos = cols
            .iter()
            .position(|c| c.id.as_deref() == Some(id.as_str()))
            .ok_or_else(|| {
                AppError::invalid(format!("key column '{id}' does not exist in the source"))
            })?;
        positions.push(pos);
    }

    let mut by_key: HashMap<CompositeKey, Vec<u64>> = HashMap::new();
    let mut offset = 0u64;
    loop {
        let rows = source.read_rows(offset, DEFAULT_WINDOW, ctx)?;
        if rows.is_empty() {
            break;
        }
        for (i, row) in rows.iter().enumerate() {
            let key = composite_key(row, &positions, &spec.normalization);
            by_key.entry(key).or_default().push(offset + i as u64);
        }
        let n = rows.len();
        offset += n as u64;
        if n < DEFAULT_WINDOW {
            break;
        }
    }

    let mut duplicates: Vec<DuplicateKey> = by_key
        .iter()
        .filter(|(_, rows)| rows.len() > 1)
        .map(|(key, rows)| DuplicateKey {
            key: key.clone(),
            rows: rows.clone(),
        })
        .collect();
    duplicates.sort_by_key(|d| d.rows[0]);

    Ok(KeyIndex {
        by_key,
        duplicates,
        rows_indexed: offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobRegistry;
    use crate::tabular::{MemSource, TabularColumn};

    fn norm(trim: bool, case_fold: bool, unicode_nfkc: bool) -> KeyNormalization {
        KeyNormalization {
            trim,
            case_fold,
            unicode_nfkc,
        }
    }

    fn col(id: &str) -> TabularColumn {
        TabularColumn {
            name: id.to_uppercase(),
            id: Some(id.to_string()),
            schema: None,
        }
    }

    fn source(rows: Vec<TabularRow>) -> MemSource {
        MemSource::new(vec![col("c0"), col("c1")], rows)
    }

    fn spec(columns: &[&str], normalization: KeyNormalization) -> KeySpec {
        KeySpec {
            columns: columns.iter().map(|s| s.to_string()).collect(),
            normalization,
        }
    }

    fn row(cells: &[Option<&str>]) -> TabularRow {
        cells.iter().map(|c| c.map(str::to_string)).collect()
    }

    // ----- normalization matrix ----------------------------------------------

    #[test]
    fn normalization_matrix() {
        // (input, trim, fold, nfkc, expected)
        let cases: &[(&str, bool, bool, bool, &str)] = &[
            ("  MiXeD  ", false, false, false, "  MiXeD  "),
            ("  MiXeD  ", true, false, false, "MiXeD"),
            ("  MiXeD  ", false, true, false, "  mixed  "),
            ("  MiXeD  ", true, true, false, "mixed"),
            // NFKC folds the ligature and fullwidth digits.
            ("\u{FB01}LE", false, false, true, "fiLE"),
            ("\u{FB01}LE", false, true, true, "file"),
            ("\u{FF11}\u{FF12}\u{FF13}", false, false, true, "123"),
            // Without NFKC the ligature survives (to_lowercase keeps it).
            ("\u{FB01}LE", false, true, false, "\u{FB01}le"),
            // NBSP canonicalizes to a space under NFKC; trim catches it last.
            (" x\u{A0}", true, false, true, "x"),
            // All three combined.
            ("  \u{FB01}LE\u{A0} ", true, true, true, "file"),
        ];
        for &(input, trim, fold, nfkc, expected) in cases {
            let n = norm(trim, fold, nfkc);
            assert_eq!(
                normalize_key_cell(input, &n),
                expected,
                "input {input:?} with {n:?}"
            );
            // Deterministic: a second pass agrees.
            assert_eq!(normalize_key_cell(input, &n), normalize_key_cell(input, &n));
        }
    }

    #[test]
    fn composite_keys_keep_missing_and_empty_distinct() {
        let n = norm(true, true, true);
        let a = composite_key(&row(&[Some("x"), None]), &[0, 1], &n);
        let b = composite_key(&row(&[Some("x"), Some("")]), &[0, 1], &n);
        assert_ne!(a, b, "missing key cell != empty key cell");

        // A short row's absent cell is missing, like an explicit None.
        let c = composite_key(&row(&[Some("x")]), &[0, 1], &n);
        assert_eq!(a, c);
    }

    // ----- content hashing ----------------------------------------------------

    #[test]
    fn content_hash_is_boundary_safe() {
        let ab_c = row(&[Some("ab"), Some("c")]);
        let a_bc = row(&[Some("a"), Some("bc")]);
        assert_ne!(row_content_hash(&ab_c), row_content_hash(&a_bc));

        let missing = row(&[None]);
        let empty = row(&[Some("")]);
        assert_ne!(row_content_hash(&missing), row_content_hash(&empty));

        // One empty-ish cell vs none at all.
        assert_ne!(row_content_hash(&row(&[])), row_content_hash(&missing));

        // Stable across calls; sensitive to any change.
        assert_eq!(row_content_hash(&ab_c), row_content_hash(&ab_c));
        assert_ne!(
            row_content_hash(&ab_c),
            row_content_hash(&row(&[Some("ab"), Some("d")]))
        );

        let hex = row_content_hash_hex(&ab_c);
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit()));
    }

    // ----- the resolver -------------------------------------------------------

    #[test]
    fn resolver_reports_every_duplicate_row() {
        let s = source(vec![
            row(&[Some("1"), Some("Alice")]),
            row(&[Some("2"), Some("alice  ")]),
            row(&[Some("3"), Some("bob")]),
            row(&[Some("4"), Some("ALICE")]),
        ]);
        let index = build_key_index(&s, &spec(&["c1"], norm(true, true, false)), None).unwrap();
        assert_eq!(index.rows_indexed, 4);
        assert!(index.has_duplicates());
        assert_eq!(index.duplicates.len(), 1);
        let dup = &index.duplicates[0];
        assert_eq!(
            dup.rows,
            vec![0, 1, 3],
            "EVERY involved row is flagged, not just the later ones"
        );
        assert_eq!(dup.key, CompositeKey(vec![Some("alice".into())]));

        // Lookup surfaces the ambiguity too.
        assert_eq!(index.unique_row(&dup.key), Err(&[0u64, 1, 3][..]));
        let bob = CompositeKey(vec![Some("bob".into())]);
        assert_eq!(index.unique_row(&bob), Ok(Some(2)));
        let ghost = CompositeKey(vec![Some("nobody".into())]);
        assert_eq!(index.unique_row(&ghost), Ok(None));
        assert!(index.rows_for(&ghost).is_empty());
    }

    #[test]
    fn resolver_composite_keys_and_order_matter() {
        let s = source(vec![
            row(&[Some("a"), Some("b")]),
            row(&[Some("b"), Some("a")]),
        ]);
        // Two-column key: (a,b) != (b,a), so no duplicates.
        let index =
            build_key_index(&s, &spec(&["c0", "c1"], norm(false, false, false)), None).unwrap();
        assert!(!index.has_duplicates());
        assert_eq!(
            index.rows_for(&CompositeKey(vec![Some("a".into()), Some("b".into())])),
            &[0]
        );
    }

    #[test]
    fn resolver_missing_and_empty_keys_stay_distinct() {
        let s = source(vec![row(&[Some("1"), None]), row(&[Some("2"), Some("")])]);
        let index = build_key_index(&s, &spec(&["c1"], norm(true, true, true)), None).unwrap();
        assert!(
            !index.has_duplicates(),
            "a missing key never collides with an empty one"
        );
    }

    #[test]
    fn resolver_validates_the_spec() {
        let s = source(vec![row(&[Some("1"), Some("x")])]);
        assert!(build_key_index(&s, &spec(&[], norm(false, false, false)), None).is_err());
        let err =
            build_key_index(&s, &spec(&["nope"], norm(false, false, false)), None).unwrap_err();
        assert!(err.to_string().contains("nope"), "unexpected error: {err}");
    }

    #[test]
    fn resolver_observes_cancellation() {
        let registry = JobRegistry::default();
        let ctx = registry.begin("test", None, |_| {});
        registry.cancel(ctx.id);
        let s = source(vec![row(&[Some("1"), Some("x")])]);
        let result = build_key_index(&s, &spec(&["c0"], norm(false, false, false)), Some(&ctx));
        assert!(matches!(result, Err(AppError::Cancelled)));
    }

    // ----- editor row ids -----------------------------------------------------

    #[test]
    fn row_ids_start_positional_and_mint_fresh_on_insert() {
        let mut ids = RowIds::new(3);
        assert_eq!(ids.len(), 3);
        assert_eq!(ids.id_at(2), Some(2));

        let minted = ids.insert(1, 2).unwrap();
        assert_eq!(minted, vec![3, 4]);
        assert_eq!(ids.id_at(0), Some(0));
        assert_eq!(ids.id_at(1), Some(3));
        assert_eq!(ids.id_at(2), Some(4));
        assert_eq!(ids.id_at(3), Some(1));
        assert_eq!(ids.position_of(2), Some(4));
        assert!(ids.insert(99, 1).is_err());
    }

    #[test]
    fn row_ids_remove_restore_round_trips_and_never_reuses() {
        let mut ids = RowIds::new(5);
        let before = ids.clone();
        let removed = ids.remove(&[3, 1]).unwrap();
        assert_eq!(removed, vec![(1, 1), (3, 3)], "ascending (position, id)");
        assert_eq!(ids.len(), 3);
        assert_eq!(ids.id_at(1), Some(2));

        // Undo restores identities verbatim.
        ids.restore(&removed).unwrap();
        assert_eq!(ids, before);

        // Fresh mints skip every id ever handed out, including removed ones.
        ids.remove(&[4]).unwrap();
        let minted = ids.insert(0, 1).unwrap();
        assert_eq!(minted, vec![5], "removed id 4 is never reused");
        assert!(ids.remove(&[99]).is_err());
    }

    #[test]
    fn row_ids_move_keeps_identities() {
        let mut ids = RowIds::new(4);
        ids.move_row(0, 2).unwrap();
        assert_eq!(ids.id_at(0), Some(1));
        assert_eq!(ids.id_at(2), Some(0));
        // The inverse move restores the original order.
        ids.move_row(2, 0).unwrap();
        assert_eq!(ids, RowIds::new(4));
        assert!(ids.move_row(9, 0).is_err());
    }

    // ----- serde --------------------------------------------------------------

    #[test]
    fn row_identity_serializes_round_trip() {
        for identity in [
            RowIdentity::EditorRow { row_uid: 7 },
            RowIdentity::SourceRecord { record: 42 },
            RowIdentity::Key {
                key: CompositeKey(vec![Some("a".into()), None]),
            },
        ] {
            let json = serde_json::to_string(&identity).unwrap();
            let back: RowIdentity = serde_json::from_str(&json).unwrap();
            assert_eq!(identity, back);
        }
    }
}
