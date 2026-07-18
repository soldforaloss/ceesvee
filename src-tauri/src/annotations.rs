//! Row bookmarks, tags and notes (F40): mark and annotate records WITHOUT
//! touching source data.
//!
//! Annotations are pure metadata. They live in an [`AnnotationStore`] keyed by
//! document id (managed by Tauri, [`AnnotationRegistry`]) — deliberately OUTSIDE
//! the [`crate::document::Document`], so they survive the whole-`Document`
//! replacement a reparse performs (the id stays the same) and never make the
//! document dirty or enter its undo stack. Persistence is either the active
//! project's `annotations` section (F37) or a document-specific sidecar file
//! (`<file>.ceesvee-notes.json`); they are NEVER written into the CSV unless
//! explicitly exported.
//!
//! ## Row identity (built on [`crate::row_identity`])
//!
//! Every annotated row is pinned by a [`RowAnchor`]: a [`RowIdentity`] plus the
//! row's content fingerprint captured at annotation time. Two anchoring
//! mechanisms are produced, in order of strength:
//!
//! 1. **Composite key** — when the store carries a [`KeySpec`] (user-selected
//!    key columns), a new anchor is a normalized [`CompositeKey`]. Survives row
//!    reordering; a duplicated key is reported ambiguous, never silently
//!    first-wins.
//! 2. **Source record + content fingerprint** — otherwise the anchor is the
//!    0-based record number plus a SHA-256 of the row. On reparse or edit the
//!    [`rematch`](AnnotationStore::rematch) engine verifies the record still
//!    holds the same content, and otherwise searches for the content elsewhere
//!    (a unique hit re-attaches; multiple hits are ambiguous; none is orphaned).
//!
//! A brand-new editable document's rows have no distinguishing content, so a
//! blank row that moves cannot be re-found — it is reported orphaned rather than
//! silently mis-attached. (The [`crate::row_identity::RowIds`] editor-id
//! mechanism that would pin such rows exactly is intentionally NOT wired into
//! the document's mutation paths in this stage — see the module notes in the
//! handoff.) The engine's contract is the same either way: **never silently
//! attach a note to an uncertain row.**
//!
//! ## What this module owns
//!
//! the annotation model (row marks, notes, cell notes, the tag namespace with
//! usage counts, author label and created/updated timestamps), the store with
//! its own revision, the rematch engine (matched / ambiguous / orphaned), the
//! annotation-state filter predicates, tag-to-column preview + application (via
//! the document's existing batched ops, one undo group), JSON/CSV export, and
//! the versioned sidecar / project-section persistence envelope.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};
use crate::job::JobCtx;
use crate::row_identity::{
    build_key_index, composite_key, row_content_hash_hex, CompositeKey, KeySpec, RowIdentity,
};
use crate::tabular::{TabularColumn, TabularSource};

/// Import/export + sidecar/project-section envelope version. Bumped only on an
/// incompatible format change; unknown fields within a version are tolerated.
pub const ANNOTATIONS_VERSION: u32 = 1;

/// Canonical suffix of a document-specific sidecar file, appended to the full
/// source file name (`orders.csv` → `orders.csv.ceesvee-notes.json`).
pub const SIDECAR_SUFFIX: &str = ".ceesvee-notes.json";

/// Wall-clock milliseconds since the Unix epoch (0 on a clock error).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// A dated free-text note with an optional author label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    pub created_ms: u64,
    pub updated_ms: u64,
}

impl Note {
    fn new(text: String, author: Option<String>) -> Note {
        let now = now_ms();
        Note {
            text,
            author,
            created_ms: now,
            updated_ms: now,
        }
    }

    /// Update text (and author), preserving the original creation time.
    fn edit(&mut self, text: String, author: Option<String>) {
        self.text = text;
        self.author = author;
        self.updated_ms = now_ms();
    }
}

/// How one annotated row is pinned to a record, plus the content fingerprint
/// (hex SHA-256) captured at annotation time for the record-anchor rematch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowAnchor {
    pub identity: RowIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
}

impl RowAnchor {
    /// Short wire tag for the anchoring mechanism ("key" / "record" / "editor").
    pub fn kind(&self) -> &'static str {
        match self.identity {
            RowIdentity::Key { .. } => "key",
            RowIdentity::SourceRecord { .. } => "record",
            RowIdentity::EditorRow { .. } => "editor",
        }
    }
}

/// One annotated row: its anchor, marks (star / flag / tags / row note), any
/// per-column cell notes, and timestamps. Kept only while it carries at least
/// one live annotation ([`RowEntry::is_empty`] prunes it otherwise).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowEntry {
    /// Stable in-session handle, preserved across rematches and round-trips.
    pub handle: u64,
    pub anchor: RowAnchor,
    #[serde(default)]
    pub star: bool,
    #[serde(default)]
    pub flag: bool,
    /// Tag names applied to this row (deduped, insertion order).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<Note>,
    /// Per-column notes keyed by stable column id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cell_notes: BTreeMap<String, Note>,
    pub created_ms: u64,
    pub updated_ms: u64,
}

impl RowEntry {
    fn new(handle: u64, anchor: RowAnchor) -> RowEntry {
        let now = now_ms();
        RowEntry {
            handle,
            anchor,
            star: false,
            flag: false,
            tags: Vec::new(),
            note: None,
            cell_notes: BTreeMap::new(),
            created_ms: now,
            updated_ms: now,
        }
    }

    /// Whether the entry carries no annotation at all (safe to drop).
    pub fn is_empty(&self) -> bool {
        !self.star
            && !self.flag
            && self.tags.is_empty()
            && self.note.is_none()
            && self.cell_notes.is_empty()
    }

    fn touch(&mut self) {
        self.updated_ms = now_ms();
    }
}

/// A tag definition in the per-document namespace: a name plus optional
/// presentation. Usage counts are computed from the row entries, never stored.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TagDef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// The star/flag/tag mark edit applied to a row in one call. Absent fields are
/// left unchanged; `add_tags` / `remove_tags` mutate the tag set.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RowMarkPatch {
    pub star: Option<bool>,
    pub flag: Option<bool>,
    pub add_tags: Vec<String>,
    pub remove_tags: Vec<String>,
}

// ---------------------------------------------------------------------------
// Store
// ---------------------------------------------------------------------------

/// One document's annotations: the row entries, the tag namespace, an optional
/// key spec (turns new anchors into composite keys), a default author label and
/// its own revision. The revision moves on every annotation edit and is the
/// guard for deferred annotation operations — independent of the document's
/// data/schema/dictionary revisions, so annotating never dirties the document.
#[derive(Debug, Clone, Default)]
pub struct AnnotationStore {
    revision: u64,
    author: Option<String>,
    key_spec: Option<KeySpec>,
    tags: BTreeMap<String, TagDef>,
    /// Row entries keyed by their stable handle.
    rows: BTreeMap<u64, RowEntry>,
    next_handle: u64,
}

impl AnnotationStore {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn author(&self) -> Option<&str> {
        self.author.as_deref()
    }

    pub fn key_spec(&self) -> Option<&KeySpec> {
        self.key_spec.as_ref()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.tags.is_empty()
    }

    /// Guard an annotation-dependent deferred edit: fail with
    /// [`AppError::StaleAnnotationsRevision`] when the store moved since
    /// `expected` was captured.
    pub fn check_revision(&self, expected: u64) -> AppResult<()> {
        if self.revision == expected {
            Ok(())
        } else {
            Err(AppError::StaleAnnotationsRevision {
                expected,
                actual: self.revision,
            })
        }
    }

    fn bump(&mut self) {
        self.revision += 1;
    }

    /// Set (or clear, with `None`) the default author label carried on new
    /// notes. Existing notes are untouched.
    pub fn set_author(&mut self, author: Option<String>) {
        let author = author.and_then(|a| {
            let t = a.trim();
            (!t.is_empty()).then(|| t.to_string())
        });
        if self.author != author {
            self.author = author;
            self.bump();
        }
    }

    /// Set (or clear, with `None`) the key columns used to anchor NEW
    /// annotations. Existing anchors keep their captured form until re-anchored
    /// by a rematch that upgrades them — see [`AnnotationStore::reanchor`].
    pub fn set_key_spec(&mut self, key_spec: Option<KeySpec>) {
        let key_spec = key_spec.filter(|k| !k.columns.is_empty());
        if self.key_spec != key_spec {
            self.key_spec = key_spec;
            self.bump();
        }
    }

    // ----- tag namespace ---------------------------------------------------

    /// Define or update a tag in the namespace.
    pub fn define_tag(&mut self, def: TagDef) -> AppResult<()> {
        let name = def.name.trim().to_string();
        if name.is_empty() {
            return Err(AppError::invalid("a tag needs a name"));
        }
        self.tags.insert(
            name.clone(),
            TagDef {
                name,
                color: def.color,
                description: def.description,
            },
        );
        self.bump();
        Ok(())
    }

    /// Remove a tag from the namespace AND from every row that carries it.
    pub fn remove_tag(&mut self, name: &str) {
        let existed = self.tags.remove(name).is_some();
        let mut changed = existed;
        let mut emptied = Vec::new();
        for (handle, entry) in self.rows.iter_mut() {
            let before = entry.tags.len();
            entry.tags.retain(|t| t != name);
            if entry.tags.len() != before {
                entry.touch();
                changed = true;
                if entry.is_empty() {
                    emptied.push(*handle);
                }
            }
        }
        for handle in emptied {
            self.rows.remove(&handle);
        }
        if changed {
            self.bump();
        }
    }

    /// Ensure a tag exists in the namespace (auto-created when first applied).
    fn ensure_tag(&mut self, name: &str) {
        self.tags.entry(name.to_string()).or_insert_with(|| TagDef {
            name: name.to_string(),
            color: None,
            description: None,
        });
    }

    // ----- row lifecycle ---------------------------------------------------

    fn fresh_handle(&mut self) -> u64 {
        let h = self.next_handle;
        self.next_handle += 1;
        h
    }

    /// Resolve a key column spec to positions in `columns` (by stable id).
    fn key_positions(columns: &[TabularColumn], spec: &KeySpec) -> AppResult<Vec<usize>> {
        let mut positions = Vec::with_capacity(spec.columns.len());
        for id in &spec.columns {
            let pos = columns
                .iter()
                .position(|c| c.id.as_deref() == Some(id.as_str()))
                .ok_or_else(|| {
                    AppError::invalid(format!(
                        "annotation key column '{id}' does not exist in the source"
                    ))
                })?;
            positions.push(pos);
        }
        Ok(positions)
    }

    /// Capture an anchor for the row at absolute `record`: a composite key when
    /// a key spec is set, else the record number; content fingerprint always.
    pub fn capture_anchor(
        &self,
        source: &dyn TabularSource,
        record: u64,
        ctx: Option<&JobCtx>,
    ) -> AppResult<RowAnchor> {
        let row = source
            .read_rows(record, 1, ctx)?
            .into_iter()
            .next()
            .ok_or_else(|| AppError::invalid("row is out of range"))?;
        let content_hash = Some(row_content_hash_hex(&row));
        let identity = match &self.key_spec {
            Some(spec) => {
                let positions = Self::key_positions(&source.columns(), spec)?;
                RowIdentity::Key {
                    key: composite_key(&row, &positions, &spec.normalization),
                }
            }
            None => RowIdentity::SourceRecord { record },
        };
        Ok(RowAnchor {
            identity,
            content_hash,
        })
    }

    /// The handle of the entry currently resolved to absolute `record`, if any.
    fn handle_at(resolution: &Resolution, record: u64) -> Option<u64> {
        resolution.by_handle.iter().find_map(|(handle, r)| {
            (r.status == MatchStatus::Matched && r.record == Some(record)).then_some(*handle)
        })
    }

    /// The entry attached to absolute `record`, creating a fresh one (with a
    /// captured anchor) when none exists there. Rematches first so the lookup
    /// reflects the current document state.
    fn entry_for_record(
        &mut self,
        source: &dyn TabularSource,
        record: u64,
        ctx: Option<&JobCtx>,
    ) -> AppResult<u64> {
        let resolution = self.rematch(source, ctx)?;
        if let Some(handle) = Self::handle_at(&resolution, record) {
            return Ok(handle);
        }
        let anchor = self.capture_anchor(source, record, ctx)?;
        let handle = self.fresh_handle();
        self.rows.insert(handle, RowEntry::new(handle, anchor));
        Ok(handle)
    }

    fn prune_if_empty(&mut self, handle: u64) {
        if self.rows.get(&handle).is_some_and(RowEntry::is_empty) {
            self.rows.remove(&handle);
        }
    }

    // ----- row edits -------------------------------------------------------

    /// Apply a star/flag/tag mark patch to the row at absolute `record`.
    pub fn edit_row_marks(
        &mut self,
        source: &dyn TabularSource,
        record: u64,
        patch: &RowMarkPatch,
        ctx: Option<&JobCtx>,
    ) -> AppResult<()> {
        let handle = self.entry_for_record(source, record, ctx)?;
        for tag in &patch.add_tags {
            let tag = tag.trim();
            if !tag.is_empty() {
                self.ensure_tag(tag);
            }
        }
        let entry = self.rows.get_mut(&handle).expect("just created/found");
        if let Some(star) = patch.star {
            entry.star = star;
        }
        if let Some(flag) = patch.flag {
            entry.flag = flag;
        }
        for tag in &patch.add_tags {
            let tag = tag.trim().to_string();
            if !tag.is_empty() && !entry.tags.contains(&tag) {
                entry.tags.push(tag);
            }
        }
        if !patch.remove_tags.is_empty() {
            entry.tags.retain(|t| !patch.remove_tags.contains(t));
        }
        entry.touch();
        self.prune_if_empty(handle);
        self.bump();
        Ok(())
    }

    /// Set (`Some`) or clear (`None`) the ROW note on the row at `record`.
    pub fn set_row_note(
        &mut self,
        source: &dyn TabularSource,
        record: u64,
        text: Option<String>,
        author: Option<String>,
        ctx: Option<&JobCtx>,
    ) -> AppResult<()> {
        let author = author.or_else(|| self.author.clone());
        let handle = self.entry_for_record(source, record, ctx)?;
        let entry = self.rows.get_mut(&handle).expect("just created/found");
        apply_note(&mut entry.note, text, author);
        entry.touch();
        self.prune_if_empty(handle);
        self.bump();
        Ok(())
    }

    /// Set (`Some`) or clear (`None`) a CELL note on `column_id` of the row at
    /// `record`.
    pub fn set_cell_note(
        &mut self,
        source: &dyn TabularSource,
        record: u64,
        column_id: &str,
        text: Option<String>,
        author: Option<String>,
        ctx: Option<&JobCtx>,
    ) -> AppResult<()> {
        if column_id.trim().is_empty() {
            return Err(AppError::invalid("a cell note needs a column id"));
        }
        let author = author.or_else(|| self.author.clone());
        let handle = self.entry_for_record(source, record, ctx)?;
        let entry = self.rows.get_mut(&handle).expect("just created/found");
        match text {
            Some(text) if !text.trim().is_empty() => match entry.cell_notes.get_mut(column_id) {
                Some(note) => note.edit(text, author),
                None => {
                    entry
                        .cell_notes
                        .insert(column_id.to_string(), Note::new(text, author));
                }
            },
            _ => {
                entry.cell_notes.remove(column_id);
            }
        }
        entry.touch();
        self.prune_if_empty(handle);
        self.bump();
        Ok(())
    }

    /// Delete one whole row entry (by handle). Returns whether it existed.
    pub fn remove_row(&mut self, handle: u64) -> bool {
        let removed = self.rows.remove(&handle).is_some();
        if removed {
            self.bump();
        }
        removed
    }

    /// Discard every orphaned entry (identified against the current source).
    pub fn discard_orphans(
        &mut self,
        source: &dyn TabularSource,
        ctx: Option<&JobCtx>,
    ) -> AppResult<usize> {
        let resolution = self.rematch(source, ctx)?;
        let orphans: Vec<u64> = resolution
            .by_handle
            .iter()
            .filter(|(_, r)| r.status == MatchStatus::Orphaned)
            .map(|(h, _)| *h)
            .collect();
        for handle in &orphans {
            self.rows.remove(handle);
        }
        if !orphans.is_empty() {
            self.bump();
        }
        Ok(orphans.len())
    }

    /// Re-capture every MATCHED entry's anchor against the current source under
    /// the current key spec — the way a newly-set key spec is adopted by
    /// existing annotations. Ambiguous / orphaned entries keep their anchor so
    /// the review list still explains them.
    pub fn reanchor(&mut self, source: &dyn TabularSource, ctx: Option<&JobCtx>) -> AppResult<()> {
        let resolution = self.rematch(source, ctx)?;
        let mut updates: Vec<(u64, RowAnchor)> = Vec::new();
        for (handle, res) in &resolution.by_handle {
            if res.status == MatchStatus::Matched {
                if let Some(record) = res.record {
                    updates.push((*handle, self.capture_anchor(source, record, ctx)?));
                }
            }
        }
        let mut changed = false;
        for (handle, anchor) in updates {
            if let Some(entry) = self.rows.get_mut(&handle) {
                if entry.anchor != anchor {
                    entry.anchor = anchor;
                    changed = true;
                }
            }
        }
        if changed {
            self.bump();
        }
        Ok(())
    }

    // ----- rematch engine --------------------------------------------------

    /// Resolve every entry against the current `source`: matched (unique row),
    /// ambiguous (duplicate key / duplicate content) or orphaned (no row).
    /// A pure read of `self` + `source`; callers persist nothing from it beyond
    /// the returned map.
    pub fn rematch(
        &self,
        source: &dyn TabularSource,
        ctx: Option<&JobCtx>,
    ) -> AppResult<Resolution> {
        let mut by_handle: BTreeMap<u64, ResolvedRow> = BTreeMap::new();

        // A key index is needed only when some entry is key-anchored AND a key
        // spec is configured. Without a spec, key anchors cannot be resolved.
        let needs_key = self
            .rows
            .values()
            .any(|e| matches!(e.anchor.identity, RowIdentity::Key { .. }));
        let key_index = match (needs_key, &self.key_spec) {
            (true, Some(spec)) => Some(build_key_index(source, spec, ctx)?),
            _ => None,
        };

        // Record anchors: verify the original record still holds the captured
        // content before any scan; collect the ones that drifted.
        let mut drifted: Vec<(u64, String)> = Vec::new();
        for (handle, entry) in &self.rows {
            match &entry.anchor.identity {
                RowIdentity::Key { key } => {
                    by_handle.insert(*handle, resolve_key(key, key_index.as_ref()));
                }
                RowIdentity::SourceRecord { record } => {
                    let resolved = self.resolve_record_direct(source, *record, entry, ctx)?;
                    match resolved {
                        Some(res) => {
                            by_handle.insert(*handle, res);
                        }
                        None => match &entry.anchor.content_hash {
                            // Original record drifted; defer to a content search.
                            Some(hash) => drifted.push((*handle, hash.clone())),
                            None => {
                                by_handle.insert(*handle, ResolvedRow::orphaned());
                            }
                        },
                    }
                }
                // Editor-row anchors are not produced in this stage (the RowIds
                // mechanism is not wired into the document); treat as orphaned.
                RowIdentity::EditorRow { .. } => {
                    by_handle.insert(*handle, ResolvedRow::orphaned());
                }
            }
        }

        // One content-hash scan resolves everything that drifted.
        if !drifted.is_empty() {
            let content = self.content_index(source, ctx)?;
            for (handle, hash) in drifted {
                let res = match content.get(&hash).map(Vec::as_slice) {
                    None | Some([]) => ResolvedRow::orphaned(),
                    Some([one]) => ResolvedRow::matched(*one),
                    Some(many) => ResolvedRow::ambiguous(many.to_vec()),
                };
                by_handle.insert(handle, res);
            }
        }

        Ok(Resolution { by_handle })
    }

    /// Resolve a record anchor by re-reading its original record: `Some(row)`
    /// when the content still matches (or no hash was stored), `None` when it
    /// drifted (caller falls back to a content search).
    fn resolve_record_direct(
        &self,
        source: &dyn TabularSource,
        record: u64,
        entry: &RowEntry,
        ctx: Option<&JobCtx>,
    ) -> AppResult<Option<ResolvedRow>> {
        let row = source.read_rows(record, 1, ctx)?.into_iter().next();
        let Some(row) = row else {
            return Ok(None); // record past the end now
        };
        match &entry.anchor.content_hash {
            Some(hash) if &row_content_hash_hex(&row) != hash => Ok(None),
            _ => Ok(Some(ResolvedRow::matched(record))),
        }
    }

    /// Content-hash → record numbers over the whole source (one streamed pass).
    fn content_index(
        &self,
        source: &dyn TabularSource,
        ctx: Option<&JobCtx>,
    ) -> AppResult<HashMap<String, Vec<u64>>> {
        use crate::tabular::DEFAULT_WINDOW;
        let mut map: HashMap<String, Vec<u64>> = HashMap::new();
        let mut offset = 0u64;
        loop {
            let rows = source.read_rows(offset, DEFAULT_WINDOW, ctx)?;
            if rows.is_empty() {
                break;
            }
            for (i, row) in rows.iter().enumerate() {
                map.entry(row_content_hash_hex(row))
                    .or_default()
                    .push(offset + i as u64);
            }
            let n = rows.len();
            offset += n as u64;
            if n < DEFAULT_WINDOW {
                break;
            }
        }
        Ok(map)
    }

    // ----- read views (over a resolution) ----------------------------------

    /// Tag namespace with per-tag usage counts across all row entries.
    fn tag_usage(&self) -> Vec<TagUsage> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for entry in self.rows.values() {
            for tag in &entry.tags {
                *counts.entry(tag.as_str()).or_default() += 1;
            }
        }
        self.tags
            .values()
            .map(|def| TagUsage {
                name: def.name.clone(),
                color: def.color.clone(),
                description: def.description.clone(),
                count: counts.get(def.name.as_str()).copied().unwrap_or(0),
            })
            .collect()
    }

    /// The panel surface: every annotation with its current resolution status
    /// and resolved record (for jump-to-row), plus the tag namespace and
    /// match tallies. `doc_revision` is echoed for guarding downstream
    /// document ops (filter, tag-to-column).
    pub fn view(
        &self,
        source: &dyn TabularSource,
        doc_revision: u64,
        ctx: Option<&JobCtx>,
    ) -> AppResult<AnnotationsView> {
        let resolution = self.rematch(source, ctx)?;
        let (mut matched, mut ambiguous, mut orphaned) = (0usize, 0usize, 0usize);
        let mut entries: Vec<RowAnnotationView> = Vec::with_capacity(self.rows.len());
        for (handle, entry) in &self.rows {
            let res = resolution
                .by_handle
                .get(handle)
                .cloned()
                .unwrap_or_else(ResolvedRow::orphaned);
            match res.status {
                MatchStatus::Matched => matched += 1,
                MatchStatus::Ambiguous => ambiguous += 1,
                MatchStatus::Orphaned => orphaned += 1,
            }
            entries.push(RowAnnotationView {
                handle: *handle,
                status: res.status,
                record: res.record,
                candidates: res.candidates,
                anchor_kind: entry.anchor.kind(),
                star: entry.star,
                flag: entry.flag,
                tags: entry.tags.clone(),
                note: entry.note.clone(),
                cell_notes: entry
                    .cell_notes
                    .iter()
                    .map(|(column_id, note)| CellNoteView {
                        column_id: column_id.clone(),
                        note: note.clone(),
                    })
                    .collect(),
                created_ms: entry.created_ms,
                updated_ms: entry.updated_ms,
            });
        }
        // Stable order for the panel: matched (by record) first, then the
        // review items (ambiguous, orphaned) by handle.
        entries.sort_by(|a, b| match (a.record, b.record) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.handle.cmp(&b.handle),
        });
        Ok(AnnotationsView {
            annotations_revision: self.revision,
            revision: doc_revision,
            author: self.author.clone(),
            key_columns: self
                .key_spec
                .as_ref()
                .map(|k| k.columns.clone())
                .unwrap_or_default(),
            tags: self.tag_usage(),
            matched,
            ambiguous,
            orphaned,
            entries,
        })
    }

    /// The review list only: ambiguous + orphaned annotations after a rematch.
    pub fn rematch_report(
        &self,
        source: &dyn TabularSource,
        ctx: Option<&JobCtx>,
    ) -> AppResult<RematchReport> {
        let resolution = self.rematch(source, ctx)?;
        let mut matched = 0usize;
        let mut ambiguous = Vec::new();
        let mut orphaned = Vec::new();
        for (handle, entry) in &self.rows {
            let res = resolution
                .by_handle
                .get(handle)
                .cloned()
                .unwrap_or_else(ResolvedRow::orphaned);
            match res.status {
                MatchStatus::Matched => matched += 1,
                MatchStatus::Ambiguous => ambiguous.push(ReviewItem {
                    handle: *handle,
                    label: entry_label(entry),
                    candidates: res.candidates,
                }),
                MatchStatus::Orphaned => orphaned.push(ReviewItem {
                    handle: *handle,
                    label: entry_label(entry),
                    candidates: Vec::new(),
                }),
            }
        }
        Ok(RematchReport {
            annotations_revision: self.revision,
            matched,
            ambiguous,
            orphaned,
        })
    }

    // ----- filter predicates ----------------------------------------------

    /// Absolute record numbers of the MATCHED rows satisfying `predicate`, in
    /// ascending order. Ambiguous / orphaned annotations never contribute (an
    /// uncertain row is never filtered onto).
    pub fn matching_records(
        &self,
        source: &dyn TabularSource,
        predicate: &AnnotationPredicate,
        ctx: Option<&JobCtx>,
    ) -> AppResult<Vec<u64>> {
        let resolution = self.rematch(source, ctx)?;
        let mut records: Vec<u64> = Vec::new();
        for (handle, entry) in &self.rows {
            let Some(res) = resolution.by_handle.get(handle) else {
                continue;
            };
            if res.status != MatchStatus::Matched {
                continue;
            }
            if predicate.matches(entry) {
                if let Some(record) = res.record {
                    records.push(record);
                }
            }
        }
        records.sort_unstable();
        records.dedup();
        Ok(records)
    }

    // ----- tag → column ----------------------------------------------------

    /// Preview copying a tag into a column: how many matched rows carry the tag,
    /// how many annotations are skipped as ambiguous / orphaned, and a bounded
    /// sample of the record → value writes. Read-only.
    pub fn preview_tag_to_column(
        &self,
        source: &dyn TabularSource,
        tag: &str,
        doc_revision: u64,
        ctx: Option<&JobCtx>,
    ) -> AppResult<TagToColumnPreview> {
        if !self.tags.contains_key(tag) {
            return Err(AppError::invalid(format!("no such tag: '{tag}'")));
        }
        let resolution = self.rematch(source, ctx)?;
        let mut writes: Vec<(u64, String)> = Vec::new();
        let mut ambiguous_skipped = 0usize;
        let mut orphaned_skipped = 0usize;
        for (handle, entry) in &self.rows {
            if !entry.tags.iter().any(|t| t == tag) {
                continue;
            }
            match resolution.by_handle.get(handle).map(|r| r.status) {
                Some(MatchStatus::Matched) => {
                    if let Some(record) = resolution.by_handle[handle].record {
                        writes.push((record, tag.to_string()));
                    }
                }
                Some(MatchStatus::Ambiguous) => ambiguous_skipped += 1,
                _ => orphaned_skipped += 1,
            }
        }
        writes.sort_unstable_by_key(|(r, _)| *r);
        let sample = writes
            .iter()
            .take(TAG_SAMPLE)
            .map(|(record, value)| TagCellSample {
                record: *record,
                value: value.clone(),
            })
            .collect();
        Ok(TagToColumnPreview {
            revision: doc_revision,
            tag: tag.to_string(),
            rows_affected: writes.len(),
            ambiguous_skipped,
            orphaned_skipped,
            sample,
        })
    }

    /// The record → value writes for a tag over the MATCHED rows (ascending by
    /// record). Feeds the document's batched apply.
    pub fn tag_to_column_writes(
        &self,
        source: &dyn TabularSource,
        tag: &str,
        ctx: Option<&JobCtx>,
    ) -> AppResult<Vec<(u64, String)>> {
        if !self.tags.contains_key(tag) {
            return Err(AppError::invalid(format!("no such tag: '{tag}'")));
        }
        let resolution = self.rematch(source, ctx)?;
        let mut writes: Vec<(u64, String)> = Vec::new();
        for (handle, entry) in &self.rows {
            if !entry.tags.iter().any(|t| t == tag) {
                continue;
            }
            if let Some(res) = resolution.by_handle.get(handle) {
                if res.status == MatchStatus::Matched {
                    if let Some(record) = res.record {
                        writes.push((record, tag.to_string()));
                    }
                }
            }
        }
        writes.sort_unstable_by_key(|(r, _)| *r);
        Ok(writes)
    }

    // ----- persistence envelope --------------------------------------------

    /// Serialize the store into its versioned export envelope.
    pub fn to_export(&self) -> AnnotationsExport {
        AnnotationsExport {
            version: ANNOTATIONS_VERSION,
            author: self.author.clone(),
            key_spec: self.key_spec.clone(),
            tags: self.tags.values().cloned().collect(),
            entries: self.rows.values().cloned().collect(),
        }
    }

    /// Rebuild a store from an export envelope (adopting its handles; the next
    /// handle continues past the maximum so new entries never collide).
    pub fn from_export(export: AnnotationsExport) -> AnnotationStore {
        let mut tags: BTreeMap<String, TagDef> = BTreeMap::new();
        for def in export.tags {
            tags.insert(def.name.clone(), def);
        }
        let mut rows: BTreeMap<u64, RowEntry> = BTreeMap::new();
        let mut max_handle = 0u64;
        for entry in export.entries {
            max_handle = max_handle.max(entry.handle);
            // A row's tags must exist in the namespace even if the file omitted
            // the definition (forward tolerance).
            for tag in &entry.tags {
                tags.entry(tag.clone()).or_insert_with(|| TagDef {
                    name: tag.clone(),
                    color: None,
                    description: None,
                });
            }
            rows.insert(entry.handle, entry);
        }
        AnnotationStore {
            revision: 0,
            author: export.author,
            key_spec: export.key_spec.filter(|k| !k.columns.is_empty()),
            tags,
            next_handle: if rows.is_empty() { 0 } else { max_handle + 1 },
            rows,
        }
    }

    // ----- export (JSON / CSV) --------------------------------------------

    /// Render the annotations for explicit export in the requested format,
    /// resolving records against the current source so exported rows carry a
    /// stable identity and status.
    pub fn export_as(
        &self,
        source: &dyn TabularSource,
        format: AnnotationExportFormat,
        ctx: Option<&JobCtx>,
    ) -> AppResult<String> {
        match format {
            AnnotationExportFormat::Json => self.export_json(),
            AnnotationExportFormat::Csv => self.export_csv(source, ctx),
        }
    }

    fn export_json(&self) -> AppResult<String> {
        serde_json::to_string_pretty(&self.to_export())
            .map_err(|e| AppError::invalid(format!("could not serialize annotations: {e}")))
    }

    /// Flat CSV: one row per annotation, plus one row per cell note. Records are
    /// resolved against the current source; unresolved annotations still export
    /// with their status so nothing is silently dropped.
    fn export_csv(&self, source: &dyn TabularSource, ctx: Option<&JobCtx>) -> AppResult<String> {
        let resolution = self.rematch(source, ctx)?;
        let mut wtr = csv::Writer::from_writer(Vec::new());
        wtr.write_record([
            "handle",
            "status",
            "record",
            "anchorKind",
            "scope",
            "columnId",
            "star",
            "flag",
            "tags",
            "note",
            "author",
            "createdMs",
            "updatedMs",
        ])?;
        for (handle, entry) in &self.rows {
            let res = resolution
                .by_handle
                .get(handle)
                .cloned()
                .unwrap_or_else(ResolvedRow::orphaned);
            let record = res.record.map(|r| r.to_string()).unwrap_or_default();
            let status = res.status.label();
            // Row-level annotation line.
            wtr.write_record([
                handle.to_string(),
                status.to_string(),
                record.clone(),
                entry.anchor.kind().to_string(),
                "row".to_string(),
                String::new(),
                entry.star.to_string(),
                entry.flag.to_string(),
                entry.tags.join("; "),
                entry
                    .note
                    .as_ref()
                    .map(|n| n.text.clone())
                    .unwrap_or_default(),
                entry
                    .note
                    .as_ref()
                    .and_then(|n| n.author.clone())
                    .unwrap_or_default(),
                entry.created_ms.to_string(),
                entry.updated_ms.to_string(),
            ])?;
            // One line per cell note.
            for (column_id, note) in &entry.cell_notes {
                wtr.write_record([
                    handle.to_string(),
                    status.to_string(),
                    record.clone(),
                    entry.anchor.kind().to_string(),
                    "cell".to_string(),
                    column_id.clone(),
                    String::new(),
                    String::new(),
                    String::new(),
                    note.text.clone(),
                    note.author.clone().unwrap_or_default(),
                    note.created_ms.to_string(),
                    note.updated_ms.to_string(),
                ])?;
            }
        }
        let bytes = wtr
            .into_inner()
            .map_err(|e| AppError::invalid(format!("could not serialize annotations CSV: {e}")))?;
        String::from_utf8(bytes)
            .map_err(|e| AppError::invalid(format!("annotations CSV was not valid UTF-8: {e}")))
    }
}

/// Set or clear a note in place, preserving the creation time on an edit.
fn apply_note(slot: &mut Option<Note>, text: Option<String>, author: Option<String>) {
    match text {
        Some(text) if !text.trim().is_empty() => match slot {
            Some(note) => note.edit(text, author),
            None => *slot = Some(Note::new(text, author)),
        },
        _ => *slot = None,
    }
}

/// A short human label for a review-list entry (note preview, else tags/marks).
fn entry_label(entry: &RowEntry) -> String {
    if let Some(note) = &entry.note {
        let preview: String = note.text.chars().take(60).collect();
        return preview.replace('\n', " ");
    }
    if !entry.tags.is_empty() {
        return entry.tags.join(", ");
    }
    if let Some((col, note)) = entry.cell_notes.iter().next() {
        let preview: String = note.text.chars().take(48).collect();
        return format!("{col}: {}", preview.replace('\n', " "));
    }
    let mut marks = Vec::new();
    if entry.star {
        marks.push("starred");
    }
    if entry.flag {
        marks.push("flagged");
    }
    if marks.is_empty() {
        "annotation".to_string()
    } else {
        marks.join(", ")
    }
}

/// How many record→value writes a tag-to-column preview samples.
const TAG_SAMPLE: usize = 20;

// ---------------------------------------------------------------------------
// Rematch result types
// ---------------------------------------------------------------------------

/// The status of one annotation against the current source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MatchStatus {
    Matched,
    Ambiguous,
    Orphaned,
}

impl MatchStatus {
    fn label(self) -> &'static str {
        match self {
            MatchStatus::Matched => "matched",
            MatchStatus::Ambiguous => "ambiguous",
            MatchStatus::Orphaned => "orphaned",
        }
    }
}

/// One entry's resolution: status, the resolved record (when matched) and the
/// candidate records (when ambiguous).
#[derive(Debug, Clone)]
pub struct ResolvedRow {
    pub status: MatchStatus,
    pub record: Option<u64>,
    pub candidates: Vec<u64>,
}

impl ResolvedRow {
    fn matched(record: u64) -> ResolvedRow {
        ResolvedRow {
            status: MatchStatus::Matched,
            record: Some(record),
            candidates: Vec::new(),
        }
    }

    fn ambiguous(candidates: Vec<u64>) -> ResolvedRow {
        ResolvedRow {
            status: MatchStatus::Ambiguous,
            record: None,
            candidates,
        }
    }

    fn orphaned() -> ResolvedRow {
        ResolvedRow {
            status: MatchStatus::Orphaned,
            record: None,
            candidates: Vec::new(),
        }
    }
}

/// Resolve a key anchor against a (possibly absent) key index.
fn resolve_key(
    key: &CompositeKey,
    key_index: Option<&crate::row_identity::KeyIndex>,
) -> ResolvedRow {
    match key_index {
        None => ResolvedRow::orphaned(),
        Some(index) => match index.unique_row(key) {
            Ok(Some(row)) => ResolvedRow::matched(row),
            Ok(None) => ResolvedRow::orphaned(),
            Err(rows) => ResolvedRow::ambiguous(rows.to_vec()),
        },
    }
}

/// The full handle → resolution map produced by [`AnnotationStore::rematch`].
#[derive(Debug, Clone, Default)]
pub struct Resolution {
    pub by_handle: BTreeMap<u64, ResolvedRow>,
}

// ---------------------------------------------------------------------------
// Wire DTOs (camelCase)
// ---------------------------------------------------------------------------

/// A tag with its usage count across annotated rows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagUsage {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub count: usize,
}

/// One cell note in a row view.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CellNoteView {
    pub column_id: String,
    pub note: Note,
}

/// One annotation, resolved against the current document, for the panel.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowAnnotationView {
    pub handle: u64,
    pub status: MatchStatus,
    /// Absolute record number when matched.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record: Option<u64>,
    /// Candidate records when ambiguous.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<u64>,
    pub anchor_kind: &'static str,
    pub star: bool,
    pub flag: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<Note>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cell_notes: Vec<CellNoteView>,
    pub created_ms: u64,
    pub updated_ms: u64,
}

/// The full annotations surface for the front end.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationsView {
    pub annotations_revision: u64,
    /// The document revision, echoed for guarding downstream document ops.
    pub revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// The active key columns (stable ids), empty when record-anchored.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub key_columns: Vec<String>,
    pub tags: Vec<TagUsage>,
    pub matched: usize,
    pub ambiguous: usize,
    pub orphaned: usize,
    pub entries: Vec<RowAnnotationView>,
}

/// One item in the rematch review list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewItem {
    pub handle: u64,
    pub label: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<u64>,
}

/// The outcome of a rematch: tallies plus the ambiguous / orphaned review list.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RematchReport {
    pub annotations_revision: u64,
    pub matched: usize,
    pub ambiguous: Vec<ReviewItem>,
    pub orphaned: Vec<ReviewItem>,
}

/// The annotation-state filter predicate (integrates with the row-filter view).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AnnotationPredicate {
    Starred,
    Flagged,
    /// Any tag when `tag` is absent, else a specific tag.
    Tagged {
        tag: Option<String>,
    },
    /// Has a row note.
    HasNote,
    /// Has at least one cell note.
    HasCellNote,
    /// Carries any annotation at all.
    AnyAnnotation,
}

impl AnnotationPredicate {
    fn matches(&self, entry: &RowEntry) -> bool {
        match self {
            AnnotationPredicate::Starred => entry.star,
            AnnotationPredicate::Flagged => entry.flag,
            AnnotationPredicate::Tagged { tag: Some(tag) } => entry.tags.iter().any(|t| t == tag),
            AnnotationPredicate::Tagged { tag: None } => !entry.tags.is_empty(),
            AnnotationPredicate::HasNote => entry.note.is_some(),
            AnnotationPredicate::HasCellNote => !entry.cell_notes.is_empty(),
            AnnotationPredicate::AnyAnnotation => !entry.is_empty(),
        }
    }
}

/// Where a tag-to-column apply writes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum TagToColumnTarget {
    /// Create a fresh column with this header, filled for tagged rows and blank
    /// elsewhere (one undo op).
    NewColumn { name: String },
    /// Write the tag into an existing column (only the tagged rows are set).
    ExistingColumn { column: usize },
}

/// One record → value write in a tag-to-column preview sample.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagCellSample {
    pub record: u64,
    pub value: String,
}

/// Preview of copying a tag into a column (revision-guarded on apply).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagToColumnPreview {
    pub revision: u64,
    pub tag: String,
    pub rows_affected: usize,
    pub ambiguous_skipped: usize,
    pub orphaned_skipped: usize,
    pub sample: Vec<TagCellSample>,
}

/// Export formats for the explicit annotation export action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AnnotationExportFormat {
    Json,
    Csv,
}

/// The versioned persistence envelope, used both for the sidecar file and the
/// project `annotations` section. Carries no source cell values — rows are
/// referenced by identity (composite key or record number) and content hash,
/// so it passes the project's no-cell-data scan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnnotationsExport {
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_spec: Option<KeySpec>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<TagDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<RowEntry>,
}

/// Parse a versioned annotations JSON envelope (version probed first).
pub fn parse_export(json: &str) -> AppResult<AnnotationsExport> {
    #[derive(Deserialize)]
    struct VersionProbe {
        version: u32,
    }
    let probe: VersionProbe = serde_json::from_str(json)
        .map_err(|e| AppError::invalid(format!("invalid annotations JSON: {e}")))?;
    if probe.version != ANNOTATIONS_VERSION {
        return Err(AppError::invalid(format!(
            "unsupported annotations version {} (this build reads version {ANNOTATIONS_VERSION})",
            probe.version
        )));
    }
    serde_json::from_str(json)
        .map_err(|e| AppError::invalid(format!("invalid annotations JSON: {e}")))
}

/// The sidecar path for a source file: its full name plus [`SIDECAR_SUFFIX`]
/// (`.../orders.csv` → `.../orders.csv.ceesvee-notes.json`).
pub fn sidecar_path(source: &Path) -> AppResult<PathBuf> {
    let name = source
        .file_name()
        .ok_or_else(|| AppError::invalid("the source path has no file name"))?
        .to_string_lossy()
        .to_string();
    let parent = source.parent().unwrap_or_else(|| Path::new(""));
    Ok(parent.join(format!("{name}{SIDECAR_SUFFIX}")))
}

/// Load a store from a sidecar file (empty store when the file is absent).
pub fn load_sidecar(source: &Path) -> AppResult<AnnotationStore> {
    let path = sidecar_path(source)?;
    match std::fs::read_to_string(&path) {
        Ok(json) => Ok(AnnotationStore::from_export(parse_export(&json)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AnnotationStore::default()),
        Err(e) => Err(AppError::invalid(format!(
            "could not read annotations sidecar {}: {e}",
            path.display()
        ))),
    }
}

/// Write a store to its source's sidecar file (atomic). An empty store DELETES
/// the sidecar rather than leaving a stale empty file.
pub fn save_sidecar(source: &Path, store: &AnnotationStore) -> AppResult<()> {
    let path = sidecar_path(source)?;
    if store.is_empty() {
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(AppError::Io(e)),
        }
    } else {
        let json = serde_json::to_vec_pretty(&store.to_export())
            .map_err(|e| AppError::Other(format!("annotations serialization failed: {e}")))?;
        crate::save::atomic_write(&path, crate::dto::BackupPolicy::None, |f| {
            use std::io::Write;
            f.write_all(&json)?;
            Ok(json.len() as u64)
        })?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Registry (managed by Tauri)
// ---------------------------------------------------------------------------

/// Process-wide annotation stores, keyed by document id. Separate from the
/// document registry so annotations survive the whole-`Document` replacement a
/// reparse performs (the id is stable) and never touch the document's dirty
/// state or undo stack.
#[derive(Default)]
pub struct AnnotationRegistry(pub std::sync::Mutex<HashMap<u64, AnnotationStore>>);

impl AnnotationRegistry {
    /// Run `f` with mutable access to the store for `doc_id` (created on first
    /// use), returning its result.
    pub fn with<T>(&self, doc_id: u64, f: impl FnOnce(&mut AnnotationStore) -> T) -> AppResult<T> {
        let mut guard = self
            .0
            .lock()
            .map_err(|_| AppError::Other("internal annotation lock error".into()))?;
        Ok(f(guard.entry(doc_id).or_default()))
    }

    /// Like [`AnnotationRegistry::with`] but for a fallible closure, flattening
    /// the result.
    pub fn try_with<T>(
        &self,
        doc_id: u64,
        f: impl FnOnce(&mut AnnotationStore) -> AppResult<T>,
    ) -> AppResult<T> {
        let mut guard = self
            .0
            .lock()
            .map_err(|_| AppError::Other("internal annotation lock error".into()))?;
        f(guard.entry(doc_id).or_default())
    }

    /// Replace the store for `doc_id`.
    pub fn set(&self, doc_id: u64, store: AnnotationStore) -> AppResult<()> {
        let mut guard = self
            .0
            .lock()
            .map_err(|_| AppError::Other("internal annotation lock error".into()))?;
        guard.insert(doc_id, store);
        Ok(())
    }

    /// Forget a document's annotations (on close).
    pub fn remove(&self, doc_id: u64) {
        if let Ok(mut guard) = self.0.lock() {
            guard.remove(&doc_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::row_identity::KeyNormalization;
    use crate::tabular::MemSource;

    fn col(id: &str) -> TabularColumn {
        TabularColumn {
            name: id.to_uppercase(),
            id: Some(id.to_string()),
            schema: None,
        }
    }

    fn row(cells: &[&str]) -> Vec<Option<String>> {
        cells.iter().map(|c| Some(c.to_string())).collect()
    }

    /// A two-column source (id, name) from `(id, name)` pairs.
    fn source(rows: &[(&str, &str)]) -> MemSource {
        MemSource::new(
            vec![col("c0"), col("c1")],
            rows.iter().map(|(a, b)| row(&[a, b])).collect(),
        )
    }

    fn key_spec(cols: &[&str]) -> KeySpec {
        KeySpec {
            columns: cols.iter().map(|s| s.to_string()).collect(),
            normalization: KeyNormalization::default(),
        }
    }

    fn marks_star() -> RowMarkPatch {
        RowMarkPatch {
            star: Some(true),
            ..Default::default()
        }
    }

    // ----- record anchoring + rematch matrix -------------------------------

    #[test]
    fn record_anchor_survives_view_and_reports_status() {
        let s = source(&[("1", "Ada"), ("2", "Bob"), ("3", "Cy")]);
        let mut store = AnnotationStore::default();
        store.edit_row_marks(&s, 1, &marks_star(), None).unwrap();

        let view = store.view(&s, 1, None).unwrap();
        assert_eq!(view.matched, 1);
        assert_eq!(view.orphaned, 0);
        assert_eq!(view.entries[0].record, Some(1));
        assert!(view.entries[0].star);
        assert_eq!(view.entries[0].anchor_kind, "record");
    }

    #[test]
    fn record_anchor_follows_content_when_row_moves() {
        // Star record 1 ("2,Bob"), then reorder so Bob is at record 0.
        let s = source(&[("1", "Ada"), ("2", "Bob")]);
        let mut store = AnnotationStore::default();
        store.edit_row_marks(&s, 1, &marks_star(), None).unwrap();

        let reordered = source(&[("2", "Bob"), ("1", "Ada")]);
        let report = store.rematch_report(&reordered, None).unwrap();
        assert_eq!(report.matched, 1, "content found at its new record");
        let view = store.view(&reordered, 2, None).unwrap();
        assert_eq!(view.entries[0].record, Some(0));
    }

    #[test]
    fn record_anchor_orphans_when_row_deleted() {
        let s = source(&[("1", "Ada"), ("2", "Bob"), ("3", "Cy")]);
        let mut store = AnnotationStore::default();
        store.edit_row_marks(&s, 1, &marks_star(), None).unwrap(); // Bob

        // Bob removed entirely.
        let deleted = source(&[("1", "Ada"), ("3", "Cy")]);
        let report = store.rematch_report(&deleted, None).unwrap();
        assert_eq!(report.matched, 0);
        assert_eq!(report.orphaned.len(), 1);

        // Bob restored (undo): the orphan re-attaches.
        let report = store.rematch_report(&s, None).unwrap();
        assert_eq!(report.matched, 1);
        assert!(report.orphaned.is_empty());
    }

    #[test]
    fn record_anchor_ambiguous_when_content_duplicated() {
        let s = source(&[("1", "Ada"), ("2", "Bob")]);
        let mut store = AnnotationStore::default();
        store.edit_row_marks(&s, 1, &marks_star(), None).unwrap(); // "2,Bob"

        // Edit record 1 away, and duplicate its old content at two rows.
        let dup = source(&[("2", "Bob"), ("x", "y"), ("2", "Bob")]);
        let report = store.rematch_report(&dup, None).unwrap();
        assert_eq!(report.ambiguous.len(), 1, "two rows carry the old content");
        assert_eq!(report.ambiguous[0].candidates, vec![0, 2]);
    }

    // ----- keyed anchoring -------------------------------------------------

    #[test]
    fn keyed_anchor_survives_reorder_and_reports_duplicates() {
        let mut store = AnnotationStore::default();
        store.set_key_spec(Some(key_spec(&["c0"])));
        let s = source(&[("1", "Ada"), ("2", "Bob"), ("3", "Cy")]);
        store.edit_row_marks(&s, 1, &marks_star(), None).unwrap(); // key "2"
        assert_eq!(store.rows.values().next().unwrap().anchor.kind(), "key");

        // Reorder: key "2" is now at record 2 — still matched.
        let reordered = source(&[("1", "Ada"), ("3", "Cy"), ("2", "Bob")]);
        let view = store.view(&reordered, 2, None).unwrap();
        assert_eq!(view.matched, 1);
        assert_eq!(view.entries[0].record, Some(2));

        // Duplicate the key: ambiguous, every involved row flagged.
        let dup = source(&[("2", "Bob"), ("2", "Bob2"), ("9", "z")]);
        let report = store.rematch_report(&dup, None).unwrap();
        assert_eq!(report.matched, 0);
        assert_eq!(report.ambiguous.len(), 1);
        assert_eq!(report.ambiguous[0].candidates, vec![0, 1]);
    }

    #[test]
    fn keyed_anchor_orphans_when_key_absent() {
        let mut store = AnnotationStore::default();
        store.set_key_spec(Some(key_spec(&["c0"])));
        let s = source(&[("1", "Ada"), ("2", "Bob")]);
        store.edit_row_marks(&s, 1, &marks_star(), None).unwrap(); // key "2"

        let gone = source(&[("1", "Ada"), ("3", "Cy")]);
        let report = store.rematch_report(&gone, None).unwrap();
        assert_eq!(report.orphaned.len(), 1);
    }

    // ----- entry merging + pruning + notes ---------------------------------

    #[test]
    fn multiple_edits_on_one_row_share_an_entry_and_prune_when_empty() {
        let s = source(&[("1", "Ada"), ("2", "Bob")]);
        let mut store = AnnotationStore::default();
        store.edit_row_marks(&s, 0, &marks_star(), None).unwrap();
        store
            .set_row_note(&s, 0, Some("check".into()), Some("me".into()), None)
            .unwrap();
        store
            .set_cell_note(&s, 0, "c1", Some("verify name".into()), None, None)
            .unwrap();
        assert_eq!(store.rows.len(), 1, "one entry for record 0");
        let entry = store.rows.values().next().unwrap();
        assert!(entry.star);
        assert_eq!(entry.note.as_ref().unwrap().text, "check");
        assert_eq!(entry.note.as_ref().unwrap().author.as_deref(), Some("me"));
        assert_eq!(entry.cell_notes["c1"].text, "verify name");

        // Clearing everything prunes the entry.
        store
            .edit_row_marks(
                &s,
                0,
                &RowMarkPatch {
                    star: Some(false),
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        store.set_row_note(&s, 0, None, None, None).unwrap();
        store.set_cell_note(&s, 0, "c1", None, None, None).unwrap();
        assert!(store.rows.is_empty(), "empty entry pruned");
    }

    #[test]
    fn default_author_applies_to_new_notes() {
        let s = source(&[("1", "Ada")]);
        let mut store = AnnotationStore::default();
        store.set_author(Some("  Dana  ".into()));
        assert_eq!(store.author(), Some("Dana"));
        store
            .set_row_note(&s, 0, Some("hi".into()), None, None)
            .unwrap();
        let entry = store.rows.values().next().unwrap();
        assert_eq!(entry.note.as_ref().unwrap().author.as_deref(), Some("Dana"));
    }

    // ----- tags + namespace ------------------------------------------------

    #[test]
    fn tags_track_usage_and_removal_cleans_rows() {
        let s = source(&[("1", "Ada"), ("2", "Bob")]);
        let mut store = AnnotationStore::default();
        let patch = RowMarkPatch {
            add_tags: vec!["urgent".into(), "review".into()],
            ..Default::default()
        };
        store.edit_row_marks(&s, 0, &patch, None).unwrap();
        store
            .edit_row_marks(
                &s,
                1,
                &RowMarkPatch {
                    add_tags: vec!["urgent".into()],
                    ..Default::default()
                },
                None,
            )
            .unwrap();
        let view = store.view(&s, 1, None).unwrap();
        let urgent = view.tags.iter().find(|t| t.name == "urgent").unwrap();
        assert_eq!(urgent.count, 2);
        let review = view.tags.iter().find(|t| t.name == "review").unwrap();
        assert_eq!(review.count, 1);

        // Removing the tag drops it from every row and empties Bob's entry.
        store.remove_tag("urgent");
        let view = store.view(&s, 1, None).unwrap();
        assert!(view.tags.iter().all(|t| t.name != "urgent"));
        // Bob had only "urgent" → pruned; Ada keeps "review".
        assert_eq!(view.entries.len(), 1);
        assert_eq!(view.entries[0].tags, vec!["review"]);
    }

    // ----- filter predicates ----------------------------------------------

    #[test]
    fn filter_predicates_return_matched_records_only() {
        let s = source(&[("1", "Ada"), ("2", "Bob"), ("3", "Cy")]);
        let mut store = AnnotationStore::default();
        store.edit_row_marks(&s, 0, &marks_star(), None).unwrap();
        store
            .edit_row_marks(
                &s,
                2,
                &RowMarkPatch {
                    flag: Some(true),
                    add_tags: vec!["t".into()],
                    ..Default::default()
                },
                None,
            )
            .unwrap();

        assert_eq!(
            store
                .matching_records(&s, &AnnotationPredicate::Starred, None)
                .unwrap(),
            vec![0]
        );
        assert_eq!(
            store
                .matching_records(&s, &AnnotationPredicate::Flagged, None)
                .unwrap(),
            vec![2]
        );
        assert_eq!(
            store
                .matching_records(&s, &AnnotationPredicate::AnyAnnotation, None)
                .unwrap(),
            vec![0, 2]
        );
        assert_eq!(
            store
                .matching_records(
                    &s,
                    &AnnotationPredicate::Tagged {
                        tag: Some("t".into())
                    },
                    None
                )
                .unwrap(),
            vec![2]
        );
    }

    #[test]
    fn ambiguous_rows_are_never_filtered_onto() {
        let mut store = AnnotationStore::default();
        store.set_key_spec(Some(key_spec(&["c0"])));
        let s = source(&[("1", "Ada"), ("2", "Bob")]);
        store.edit_row_marks(&s, 1, &marks_star(), None).unwrap();
        let dup = source(&[("2", "x"), ("2", "y")]);
        assert!(
            store
                .matching_records(&dup, &AnnotationPredicate::Starred, None)
                .unwrap()
                .is_empty(),
            "an ambiguous row is never selected by a filter"
        );
    }

    // ----- tag → column ----------------------------------------------------

    #[test]
    fn tag_to_column_writes_matched_rows_only() {
        let s = source(&[("1", "Ada"), ("2", "Bob"), ("3", "Cy")]);
        let mut store = AnnotationStore::default();
        let tag = RowMarkPatch {
            add_tags: vec!["keep".into()],
            ..Default::default()
        };
        store.edit_row_marks(&s, 0, &tag, None).unwrap();
        store.edit_row_marks(&s, 2, &tag, None).unwrap();

        let preview = store.preview_tag_to_column(&s, "keep", 5, None).unwrap();
        assert_eq!(preview.rows_affected, 2);
        assert_eq!(preview.revision, 5);
        assert_eq!(preview.sample.len(), 2);

        let writes = store.tag_to_column_writes(&s, "keep", None).unwrap();
        assert_eq!(
            writes,
            vec![(0, "keep".to_string()), (2, "keep".to_string())]
        );
    }

    // ----- persistence round-trip ------------------------------------------

    #[test]
    fn export_round_trip_preserves_handles_and_content() {
        let s = source(&[("1", "Ada"), ("2", "Bob")]);
        let mut store = AnnotationStore::default();
        store.set_author(Some("Dana".into()));
        store.edit_row_marks(&s, 0, &marks_star(), None).unwrap();
        store
            .set_row_note(&s, 1, Some("later".into()), None, None)
            .unwrap();
        store
            .define_tag(TagDef {
                name: "keep".into(),
                color: Some("#0f0".into()),
                description: None,
            })
            .unwrap();

        let export = store.to_export();
        let json = serde_json::to_string(&export).unwrap();
        let parsed = parse_export(&json).unwrap();
        let restored = AnnotationStore::from_export(parsed);
        assert_eq!(restored.to_export(), export);
        assert_eq!(restored.author(), Some("Dana"));
        // New entries after a load never collide with restored handles.
        assert!(restored.next_handle > restored.rows.keys().copied().max().unwrap());
    }

    #[test]
    fn sidecar_round_trips_and_deletes_when_empty() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("orders.csv");
        std::fs::write(&src, "id,name\n1,Ada\n").unwrap();
        let s = source(&[("1", "Ada")]);

        let mut store = AnnotationStore::default();
        store.edit_row_marks(&s, 0, &marks_star(), None).unwrap();
        save_sidecar(&src, &store).unwrap();

        let path = sidecar_path(&src).unwrap();
        assert!(path.exists());
        assert!(path
            .to_string_lossy()
            .ends_with("orders.csv.ceesvee-notes.json"));

        let loaded = load_sidecar(&src).unwrap();
        assert_eq!(loaded.to_export(), store.to_export());

        // Saving an empty store removes the sidecar.
        save_sidecar(&src, &AnnotationStore::default()).unwrap();
        assert!(!path.exists());
        // Loading an absent sidecar is an empty store, not an error.
        assert!(load_sidecar(&src).unwrap().is_empty());
    }

    #[test]
    fn parse_export_rejects_unknown_version() {
        let json = format!(
            r#"{{"version": {}, "entries": []}}"#,
            ANNOTATIONS_VERSION + 1
        );
        assert!(parse_export(&json).is_err());
    }

    // ----- CSV export ------------------------------------------------------

    #[test]
    fn csv_export_lists_rows_and_cell_notes_with_status() {
        let s = source(&[("1", "Ada"), ("2", "Bob")]);
        let mut store = AnnotationStore::default();
        store.edit_row_marks(&s, 0, &marks_star(), None).unwrap();
        store
            .set_cell_note(&s, 0, "c1", Some("verify".into()), Some("q".into()), None)
            .unwrap();
        let csv = store
            .export_as(&s, AnnotationExportFormat::Csv, None)
            .unwrap();
        let mut reader = csv::Reader::from_reader(csv.as_bytes());
        let records: Vec<csv::StringRecord> = reader.records().map(Result::unwrap).collect();
        assert_eq!(records.len(), 2, "one row line + one cell-note line");
        assert!(records.iter().any(|r| &r[4] == "row" && &r[6] == "true"));
        let cell = records.iter().find(|r| &r[4] == "cell").unwrap();
        assert_eq!(&cell[5], "c1");
        assert_eq!(&cell[9], "verify");
        assert_eq!(&cell[10], "q");
    }

    // ----- revision independence -------------------------------------------

    #[test]
    fn annotation_revision_moves_only_on_real_changes() {
        let s = source(&[("1", "Ada")]);
        let mut store = AnnotationStore::default();
        let r0 = store.revision();
        store.edit_row_marks(&s, 0, &marks_star(), None).unwrap();
        assert!(store.revision() > r0);
        let r1 = store.revision();
        // A no-op author set (same value) does not bump.
        store.set_author(None);
        assert_eq!(store.revision(), r1);
        // A pure rematch/view never bumps the revision.
        store.view(&s, 1, None).unwrap();
        assert_eq!(store.revision(), r1);
    }
}
