//! Data dictionary (F38): human documentation of what each column MEANS,
//! linked to the F31 stable column ID so it survives renames, reorders and
//! undo/redo. The dictionary is pure metadata: it lives on the [`Document`]
//! beside the schema, has its OWN revision, and editing it never rewrites a
//! cell or makes the document dirty.
//!
//! This module owns the documentation model ([`DictionaryField`]), the
//! per-document container ([`Dictionary`]), the editor view (with technical
//! names + inferred F31 types prefilled), versioned JSON / Markdown / CSV
//! export, and the import MERGE engine — which matches incoming entries by
//! column ID (or by mapped column name when IDs are absent) and produces a
//! field-level conflict report that must be explicitly resolved before it can
//! replace anything. It also exposes two integration hooks consumed elsewhere:
//! required-documentation checks for F08 file profiles, and the sensitive
//! (confidential/restricted) column set for the F28 PII preflight.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::document::Document;
use crate::error::{AppError, AppResult};
use crate::schema::LogicalType;

/// Import/export envelope version. Bumped only on an incompatible format
/// change; unknown fields within a version are ignored (forward-tolerant).
pub const DICTIONARY_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// Documentation model (wire DTOs, camelCase)
// ---------------------------------------------------------------------------

/// The analytical role a column plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FieldRole {
    Identifier,
    Dimension,
    Measure,
    Timestamp,
    Label,
}

impl FieldRole {
    pub fn label(self) -> &'static str {
        match self {
            FieldRole::Identifier => "identifier",
            FieldRole::Dimension => "dimension",
            FieldRole::Measure => "measure",
            FieldRole::Timestamp => "timestamp",
            FieldRole::Label => "label",
        }
    }
}

/// Data-sensitivity classification, ordered least → most sensitive so
/// `>= Confidential` selects the values the PII preflight flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Sensitivity {
    Public,
    Internal,
    Confidential,
    Restricted,
}

impl Sensitivity {
    pub fn label(self) -> &'static str {
        match self {
            Sensitivity::Public => "public",
            Sensitivity::Internal => "internal",
            Sensitivity::Confidential => "confidential",
            Sensitivity::Restricted => "restricted",
        }
    }

    /// Whether this classification makes a column PII-sensitive regardless of
    /// pattern hits (confidential or restricted). Consumed by [`sensitive_columns`].
    pub fn is_sensitive(self) -> bool {
        self >= Sensitivity::Confidential
    }
}

/// One column's documentation. Every descriptive field is optional; the entry
/// is keyed by the STABLE column ID (F12), never by position or header text.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryField {
    pub column_id: String,
    /// Human-friendly name (the technical header stays the source of truth).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<FieldRole>,
    /// Unit of measure ("USD", "ms", "kg").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Where the values originate (system of record, upstream table).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitivity: Option<Sensitivity>,
    /// Enumerated permitted values, when the column is categorical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_values: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub example: Option<String>,
    /// Data owner / steward.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl DictionaryField {
    /// An empty entry for `column_id` (used to prefill the editor and as the
    /// merge base for a column with no existing documentation).
    pub fn empty(column_id: impl Into<String>) -> Self {
        DictionaryField {
            column_id: column_id.into(),
            ..DictionaryField::default()
        }
    }

    /// Whether any documentation field carries a real (non-blank) value.
    pub fn is_documented(&self) -> bool {
        ALL_FIELD_KEYS.iter().any(|&k| value_of(self, k).is_some())
    }
}

/// Every documentable field, as a closed enum: used by the merge engine, the
/// conflict report and the F08 required-documentation profile rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DictionaryFieldKey {
    DisplayName,
    Description,
    Role,
    Unit,
    Source,
    Sensitivity,
    AllowedValues,
    Example,
    Owner,
    Notes,
}

/// All field keys, in stable presentation order.
pub const ALL_FIELD_KEYS: [DictionaryFieldKey; 10] = [
    DictionaryFieldKey::DisplayName,
    DictionaryFieldKey::Description,
    DictionaryFieldKey::Role,
    DictionaryFieldKey::Unit,
    DictionaryFieldKey::Source,
    DictionaryFieldKey::Sensitivity,
    DictionaryFieldKey::AllowedValues,
    DictionaryFieldKey::Example,
    DictionaryFieldKey::Owner,
    DictionaryFieldKey::Notes,
];

impl DictionaryFieldKey {
    /// Human label, used in conflict reports, profile issues and MD/CSV headers.
    pub fn label(self) -> &'static str {
        match self {
            DictionaryFieldKey::DisplayName => "display name",
            DictionaryFieldKey::Description => "description",
            DictionaryFieldKey::Role => "role",
            DictionaryFieldKey::Unit => "unit",
            DictionaryFieldKey::Source => "source",
            DictionaryFieldKey::Sensitivity => "sensitivity",
            DictionaryFieldKey::AllowedValues => "allowed values",
            DictionaryFieldKey::Example => "example",
            DictionaryFieldKey::Owner => "owner",
            DictionaryFieldKey::Notes => "notes",
        }
    }
}

/// Canonical (trimmed, non-blank) string value of one field, or `None` when it
/// carries no real documentation. Drives presence, equality and conflict
/// display uniformly across the differently-typed fields.
fn value_of(f: &DictionaryField, key: DictionaryFieldKey) -> Option<String> {
    fn text(o: &Option<String>) -> Option<String> {
        o.as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
    }
    match key {
        DictionaryFieldKey::DisplayName => text(&f.display_name),
        DictionaryFieldKey::Description => text(&f.description),
        DictionaryFieldKey::Role => f.role.map(|r| r.label().to_string()),
        DictionaryFieldKey::Unit => text(&f.unit),
        DictionaryFieldKey::Source => text(&f.source),
        DictionaryFieldKey::Sensitivity => f.sensitivity.map(|s| s.label().to_string()),
        DictionaryFieldKey::AllowedValues => {
            let vals: Vec<&str> = f
                .allowed_values
                .iter()
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .collect();
            (!vals.is_empty()).then(|| vals.join(", "))
        }
        DictionaryFieldKey::Example => text(&f.example),
        DictionaryFieldKey::Owner => text(&f.owner),
        DictionaryFieldKey::Notes => text(&f.notes),
    }
}

/// Copy one field's raw (typed) value from `src` into `dst`.
fn copy_field(dst: &mut DictionaryField, src: &DictionaryField, key: DictionaryFieldKey) {
    match key {
        DictionaryFieldKey::DisplayName => dst.display_name = src.display_name.clone(),
        DictionaryFieldKey::Description => dst.description = src.description.clone(),
        DictionaryFieldKey::Role => dst.role = src.role,
        DictionaryFieldKey::Unit => dst.unit = src.unit.clone(),
        DictionaryFieldKey::Source => dst.source = src.source.clone(),
        DictionaryFieldKey::Sensitivity => dst.sensitivity = src.sensitivity,
        DictionaryFieldKey::AllowedValues => dst.allowed_values = src.allowed_values.clone(),
        DictionaryFieldKey::Example => dst.example = src.example.clone(),
        DictionaryFieldKey::Owner => dst.owner = src.owner.clone(),
        DictionaryFieldKey::Notes => dst.notes = src.notes.clone(),
    }
}

/// Whether `field` populates `key` with a real value.
pub fn field_present(field: &DictionaryField, key: DictionaryFieldKey) -> bool {
    value_of(field, key).is_some()
}

// ---------------------------------------------------------------------------
// Per-document container
// ---------------------------------------------------------------------------

/// A document's data dictionary: per-column entries keyed by stable column ID.
/// Columns without an entry are simply undocumented.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dictionary {
    pub fields: BTreeMap<String, DictionaryField>,
}

impl Dictionary {
    pub fn field(&self, column_id: &str) -> Option<&DictionaryField> {
        self.fields.get(column_id)
    }

    /// Insert or replace an entry (keyed by its own `column_id`).
    pub fn set(&mut self, field: DictionaryField) {
        self.fields.insert(field.column_id.clone(), field);
    }

    /// Remove an entry; returns whether one was present.
    pub fn remove(&mut self, column_id: &str) -> bool {
        self.fields.remove(column_id).is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    pub fn len(&self) -> usize {
        self.fields.len()
    }
}

/// Reject an entry with no column ID before it reaches the model.
pub fn validate_field(field: &DictionaryField) -> AppResult<()> {
    if field.column_id.trim().is_empty() {
        return Err(AppError::invalid("dictionary entry has no columnId"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Column context (technical name + inferred F31 type)
// ---------------------------------------------------------------------------

/// The technical name + inferred F31 type of a documented column in the
/// CURRENT document (falling back to the entry's own name when the column has
/// been deleted).
struct ColumnContext {
    /// Technical header when present, else the entry's display name / ID.
    name: String,
    logical_type: Option<LogicalType>,
}

fn column_context(
    doc: &Document,
    column_id: &str,
    field: Option<&DictionaryField>,
) -> ColumnContext {
    let position = doc.column_ids().iter().position(|id| id == column_id);
    let name = match position {
        Some(pos) => doc.headers()[pos].clone(),
        None => field
            .and_then(|f| f.display_name.clone())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| column_id.to_string()),
    };
    let logical_type = position
        .and_then(|_| doc.schema().column(column_id))
        .map(|s| s.logical_type);
    ColumnContext { name, logical_type }
}

// ---------------------------------------------------------------------------
// Editor view (every column, prefilled; plus orphans)
// ---------------------------------------------------------------------------

/// One column in the dictionary editor: technical name + inferred type
/// prefilled, the stored entry when documented (an empty prefill otherwise).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryEntryView {
    pub column_id: String,
    /// Current header — the technical name shown/prefilled in the editor.
    pub column_name: String,
    pub column_index: usize,
    /// Declared/inferred logical type from F31, when a schema entry exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logical_type: Option<LogicalType>,
    pub field: DictionaryField,
    /// Whether the user has actually documented this column.
    pub documented: bool,
}

/// A documented entry whose column no longer exists (reported after a delete;
/// kept until explicitly discarded, and re-attached if the column returns).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrphanEntry {
    pub column_id: String,
    /// Best-effort label (display name, else the column ID).
    pub label: String,
    pub field: DictionaryField,
}

/// The full dictionary surface for the front end: the searchable editor rows
/// (one per current column) plus any orphaned entries. `dictionaryRevision`
/// is the metadata revision (moves on documentation edits only); `revision`
/// is the ordinary document revision (which those edits never move).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryView {
    pub dictionary_revision: u64,
    pub revision: u64,
    pub entries: Vec<DictionaryEntryView>,
    pub orphans: Vec<OrphanEntry>,
}

/// Snapshot the dictionary for the editor: one row per current column (stored
/// entry or an empty prefill), plus orphaned entries.
pub fn view(doc: &Document) -> DictionaryView {
    let entries = doc
        .column_ids()
        .iter()
        .enumerate()
        .map(|(idx, id)| {
            let stored = doc.dictionary().field(id);
            let ctx = column_context(doc, id, stored);
            DictionaryEntryView {
                column_id: id.clone(),
                column_name: ctx.name,
                column_index: idx,
                logical_type: ctx.logical_type,
                field: stored
                    .cloned()
                    .unwrap_or_else(|| DictionaryField::empty(id.clone())),
                documented: stored.is_some_and(DictionaryField::is_documented),
            }
        })
        .collect();

    DictionaryView {
        dictionary_revision: doc.dictionary_revision(),
        revision: doc.revision(),
        entries,
        orphans: orphans(doc),
    }
}

/// Documented entries whose column ID is no longer present in the document.
pub fn orphans(doc: &Document) -> Vec<OrphanEntry> {
    let live: HashSet<&str> = doc.column_ids().iter().map(String::as_str).collect();
    doc.dictionary()
        .fields
        .iter()
        .filter(|(id, _)| !live.contains(id.as_str()))
        .map(|(id, field)| OrphanEntry {
            column_id: id.clone(),
            label: field
                .display_name
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| id.clone()),
            field: field.clone(),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Exports: versioned JSON, Markdown, CSV
// ---------------------------------------------------------------------------

/// One entry in the export envelope: the documentation plus the technical
/// column name captured at export time, so a later import can remap by name
/// when the target document's IDs differ.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryExportEntry {
    /// Technical header at export time (the name-remap key on import).
    pub column_name: String,
    #[serde(flatten)]
    pub field: DictionaryField,
}

/// Versioned import/export envelope: `{ "version": 1, "entries": [...] }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryExport {
    pub version: u32,
    pub entries: Vec<DictionaryExportEntry>,
}

/// An export row: the entry with its resolved current context.
struct EntryRow {
    column_id: String,
    field: DictionaryField,
    name: String,
    logical_type: Option<LogicalType>,
    orphan: bool,
}

/// Documented entries in current-column order, followed by orphans (sorted by
/// ID). Only actually-documented entries are emitted.
fn entry_rows(doc: &Document) -> Vec<EntryRow> {
    let mut rows = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for id in doc.column_ids() {
        if let Some(field) = doc.dictionary().field(id) {
            seen.insert(id.as_str());
            let ctx = column_context(doc, id, Some(field));
            rows.push(EntryRow {
                column_id: id.clone(),
                field: field.clone(),
                name: ctx.name,
                logical_type: ctx.logical_type,
                orphan: false,
            });
        }
    }
    for (id, field) in &doc.dictionary().fields {
        if !seen.contains(id.as_str()) {
            let ctx = column_context(doc, id, Some(field));
            rows.push(EntryRow {
                column_id: id.clone(),
                field: field.clone(),
                name: ctx.name,
                logical_type: ctx.logical_type,
                orphan: true,
            });
        }
    }
    rows
}

/// Build the export envelope in current-column order (names refreshed from the
/// live headers; orphans last).
pub fn build_export(doc: &Document) -> DictionaryExport {
    let entries = entry_rows(doc)
        .into_iter()
        .map(|row| DictionaryExportEntry {
            column_name: row.name,
            field: row.field,
        })
        .collect();
    DictionaryExport {
        version: DICTIONARY_VERSION,
        entries,
    }
}

/// The three documentation export formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DictionaryFormat {
    Json,
    Markdown,
    Csv,
}

/// Render the dictionary in the requested format.
pub fn export_as(doc: &Document, format: DictionaryFormat) -> AppResult<String> {
    match format {
        DictionaryFormat::Json => export_json(doc),
        DictionaryFormat::Markdown => Ok(export_markdown(doc)),
        DictionaryFormat::Csv => export_csv(doc),
    }
}

/// Serialize the dictionary to pretty, versioned JSON.
pub fn export_json(doc: &Document) -> AppResult<String> {
    serde_json::to_string_pretty(&build_export(doc))
        .map_err(|e| AppError::invalid(format!("could not serialize dictionary: {e}")))
}

fn logical_type_label(lt: LogicalType) -> &'static str {
    match lt {
        LogicalType::Text => "text",
        LogicalType::Integer => "integer",
        LogicalType::Decimal => "decimal",
        LogicalType::Float => "float",
        LogicalType::Boolean => "boolean",
        LogicalType::Date => "date",
        LogicalType::Datetime => "datetime",
        LogicalType::Uuid => "uuid",
        LogicalType::Json => "json",
    }
}

/// Markdown documentation: one section per documented column, every field
/// present (blank fields shown as an em dash so the section is complete).
pub fn export_markdown(doc: &Document) -> String {
    let mut out = String::new();
    out.push_str("# Data dictionary\n\n");
    let file = doc.meta().file_name;
    out.push_str(&format!("Source: {file}\n\n"));

    let rows = entry_rows(doc);
    if rows.is_empty() {
        out.push_str("_No columns documented yet._\n");
        return out;
    }

    for row in rows {
        let heading = if row.orphan {
            format!("## {} (`{}`) — orphaned\n\n", row.name, row.column_id)
        } else {
            format!("## {} (`{}`)\n\n", row.name, row.column_id)
        };
        out.push_str(&heading);
        out.push_str(&format!("- **Column ID:** {}\n", row.column_id));
        out.push_str(&format!("- **Technical name:** {}\n", md_cell(&row.name)));
        let ty = row
            .logical_type
            .map(logical_type_label)
            .unwrap_or("—")
            .to_string();
        out.push_str(&format!("- **Logical type (F31):** {ty}\n"));
        for key in ALL_FIELD_KEYS {
            let value = value_of(&row.field, key).unwrap_or_else(|| "—".to_string());
            out.push_str(&format!(
                "- **{}:** {}\n",
                capitalize(key.label()),
                md_cell(&value)
            ));
        }
        out.push('\n');
    }
    out
}

/// Escape the characters that would break a Markdown list line.
fn md_cell(value: &str) -> String {
    value.replace('\n', " ").replace('|', "\\|")
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// CSV documentation: one row per documented column, every field a column.
pub fn export_csv(doc: &Document) -> AppResult<String> {
    let mut wtr = csv::Writer::from_writer(Vec::new());
    wtr.write_record([
        "columnId",
        "columnName",
        "logicalType",
        "displayName",
        "description",
        "role",
        "unit",
        "source",
        "sensitivity",
        "allowedValues",
        "example",
        "owner",
        "notes",
        "orphaned",
    ])?;
    for row in entry_rows(doc) {
        let cell = |key: DictionaryFieldKey| value_of(&row.field, key).unwrap_or_default();
        wtr.write_record([
            row.column_id.clone(),
            row.name.clone(),
            row.logical_type
                .map(logical_type_label)
                .unwrap_or("")
                .to_string(),
            cell(DictionaryFieldKey::DisplayName),
            cell(DictionaryFieldKey::Description),
            cell(DictionaryFieldKey::Role),
            cell(DictionaryFieldKey::Unit),
            cell(DictionaryFieldKey::Source),
            cell(DictionaryFieldKey::Sensitivity),
            // Allowed values as a semicolon list so a comma value stays intact.
            row.field
                .allowed_values
                .iter()
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .collect::<Vec<_>>()
                .join("; "),
            cell(DictionaryFieldKey::Example),
            cell(DictionaryFieldKey::Owner),
            cell(DictionaryFieldKey::Notes),
            if row.orphan { "true" } else { "false" }.to_string(),
        ])?;
    }
    let bytes = wtr
        .into_inner()
        .map_err(|e| AppError::invalid(format!("could not serialize dictionary CSV: {e}")))?;
    String::from_utf8(bytes)
        .map_err(|e| AppError::invalid(format!("dictionary CSV was not valid UTF-8: {e}")))
}

// ---------------------------------------------------------------------------
// Import merge engine
// ---------------------------------------------------------------------------

/// How incoming entries are matched to current columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MergeMatchBy {
    /// Only by stable column ID.
    ColumnId,
    /// Only by (case-insensitive) technical column name.
    ColumnName,
    /// Prefer the column ID; fall back to the name when the ID is absent here.
    #[default]
    Auto,
}

/// Parse a versioned dictionary JSON file (version probed first so an
/// incompatible future format fails with the version message, not a shape one).
pub fn parse_import(json: &str) -> AppResult<DictionaryExport> {
    #[derive(Deserialize)]
    struct VersionProbe {
        version: u32,
    }
    let probe: VersionProbe = serde_json::from_str(json)
        .map_err(|e| AppError::invalid(format!("invalid dictionary JSON: {e}")))?;
    if probe.version != DICTIONARY_VERSION {
        return Err(AppError::invalid(format!(
            "unsupported dictionary version {} (this build reads version {DICTIONARY_VERSION})",
            probe.version
        )));
    }
    serde_json::from_str(json)
        .map_err(|e| AppError::invalid(format!("invalid dictionary JSON: {e}")))
}

/// A single field-level disagreement between an existing entry and an incoming
/// one, requiring explicit resolution before the import can replace anything.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldConflict {
    pub column_id: String,
    /// Current technical name, for display.
    pub column_name: String,
    pub field: DictionaryFieldKey,
    /// Existing value (display form).
    pub existing: String,
    /// Incoming value (display form).
    pub incoming: String,
}

/// The plan a `preview_dictionary_import` produces: what a merge would do, and
/// which conflicts block it. Computed against `dictionaryRevision`, which the
/// apply echoes back and is rejected if it has since moved.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergePlan {
    pub dictionary_revision: u64,
    pub match_by: MergeMatchBy,
    /// Number of imported entries matched to a current column.
    pub matched_columns: usize,
    /// Column IDs that would gain a brand-new entry.
    pub new_entries: Vec<String>,
    /// Field additions that apply with no conflict (existing value was blank).
    pub clean_additions: usize,
    /// Disagreements needing explicit resolution.
    pub conflicts: Vec<FieldConflict>,
    /// Imported entries (by name/ID label) that matched no current column.
    pub unmatched: Vec<String>,
}

/// Which side of a conflict wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictChoice {
    KeepExisting,
    TakeIncoming,
}

/// One explicit per-field resolution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldResolution {
    pub column_id: String,
    pub field: DictionaryFieldKey,
    pub choice: ConflictChoice,
}

/// How the import resolves conflicts. `PerField` MUST cover every reported
/// conflict; a missing one fails the apply (conflicts are never silently
/// dropped).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum MergeResolution {
    KeepAllExisting,
    TakeAllIncoming,
    PerField { resolutions: Vec<FieldResolution> },
}

impl MergeResolution {
    /// The choice for one conflict, or `None` when unresolved (only possible
    /// under `PerField`).
    fn choice_for(&self, column_id: &str, field: DictionaryFieldKey) -> Option<ConflictChoice> {
        match self {
            MergeResolution::KeepAllExisting => Some(ConflictChoice::KeepExisting),
            MergeResolution::TakeAllIncoming => Some(ConflictChoice::TakeIncoming),
            MergeResolution::PerField { resolutions } => resolutions
                .iter()
                .find(|r| r.column_id == column_id && r.field == field)
                .map(|r| r.choice),
        }
    }
}

enum MergeMode<'a> {
    /// Report conflicts; make no choices (dry run).
    Plan,
    /// Apply choices from a resolution.
    Apply(&'a MergeResolution),
}

/// Accumulated results of a merge pass.
struct MergeStats {
    matched: usize,
    new_entries: Vec<String>,
    updated_entries: usize,
    fields_added: usize,
    conflicts: Vec<FieldConflict>,
    resolved: usize,
    unresolved: Vec<FieldConflict>,
    unmatched: Vec<String>,
}

/// Outcome of resolving one imported entry to a current column.
enum Target {
    /// Resolved to exactly one current column ID.
    Matched(String),
    /// No current column matched.
    Unmatched,
    /// The name matched more than one current column. Documents do not enforce
    /// unique headers (a source CSV or an in-app rename can duplicate one), so
    /// rather than silently collapse every same-named entry onto the FIRST
    /// column — misattributing documentation with no signal — the entry is
    /// reported and left unmatched for the user to disambiguate (e.g. by ID).
    Ambiguous { name: String, count: usize },
}

/// The technical name an import entry matches on: its captured column name, or
/// its display name as a fallback.
fn match_name(entry: &DictionaryExportEntry) -> &str {
    if entry.column_name.trim().is_empty() {
        entry.field.display_name.as_deref().unwrap_or("").trim()
    } else {
        entry.column_name.trim()
    }
}

/// Current column IDs whose header equals `name` (case-insensitive, trimmed).
fn columns_named(doc: &Document, name: &str) -> Vec<String> {
    let ids = doc.column_ids();
    doc.headers()
        .iter()
        .enumerate()
        .filter(|(_, h)| h.trim().eq_ignore_ascii_case(name))
        .map(|(i, _)| ids[i].clone())
        .collect()
}

/// Resolve one imported entry to a current column ID under `match_by`.
fn resolve_target(doc: &Document, entry: &DictionaryExportEntry, match_by: MergeMatchBy) -> Target {
    let by_id = || {
        let id = entry.field.column_id.trim();
        (!id.is_empty() && doc.column_ids().iter().any(|c| c == id)).then(|| id.to_string())
    };
    let by_name = || {
        let name = match_name(entry);
        if name.is_empty() {
            return Target::Unmatched;
        }
        let mut ids = columns_named(doc, name);
        match ids.len() {
            0 => Target::Unmatched,
            1 => Target::Matched(ids.pop().expect("len checked")),
            n => Target::Ambiguous {
                name: name.to_string(),
                count: n,
            },
        }
    };
    match match_by {
        MergeMatchBy::ColumnId => by_id().map_or(Target::Unmatched, Target::Matched),
        MergeMatchBy::ColumnName => by_name(),
        // Prefer an exact ID match (always unambiguous); fall back to the name
        // only when the ID is absent here.
        MergeMatchBy::Auto => match by_id() {
            Some(id) => Target::Matched(id),
            None => by_name(),
        },
    }
}

/// Best-effort label for an imported entry that matched nothing.
fn unmatched_label(entry: &DictionaryExportEntry) -> String {
    if !entry.column_name.trim().is_empty() {
        entry.column_name.clone()
    } else if let Some(name) = entry
        .field
        .display_name
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        name.to_string()
    } else {
        entry.field.column_id.clone()
    }
}

/// Core merge pass shared by plan and apply. `working` starts from the current
/// dictionary and each imported entry is merged into it cumulatively.
fn run_merge(
    doc: &Document,
    imported: &DictionaryExport,
    match_by: MergeMatchBy,
    mode: &MergeMode<'_>,
) -> (Dictionary, MergeStats) {
    let mut working = doc.dictionary().clone();
    let mut stats = MergeStats {
        matched: 0,
        new_entries: Vec::new(),
        updated_entries: 0,
        fields_added: 0,
        conflicts: Vec::new(),
        resolved: 0,
        unresolved: Vec::new(),
        unmatched: Vec::new(),
    };

    for entry in &imported.entries {
        let target_id = match resolve_target(doc, entry, match_by) {
            Target::Matched(id) => id,
            Target::Unmatched => {
                stats.unmatched.push(unmatched_label(entry));
                continue;
            }
            Target::Ambiguous { name, count } => {
                stats.unmatched.push(format!(
                    "{name} (ambiguous — {count} columns share this name; import by column ID)"
                ));
                continue;
            }
        };
        stats.matched += 1;
        let column_name = column_context(doc, &target_id, working.field(&target_id)).name;

        let original = working.field(&target_id).cloned();
        let mut base = original
            .clone()
            .unwrap_or_else(|| DictionaryField::empty(target_id.clone()));
        // The merged entry always keys on the TARGET column's ID.
        base.column_id = target_id.clone();

        for key in ALL_FIELD_KEYS {
            let Some(incoming) = value_of(&entry.field, key) else {
                continue; // incoming blank — keep existing
            };
            match value_of(&base, key) {
                None => {
                    // Clean addition into a previously-blank field.
                    copy_field(&mut base, &entry.field, key);
                    stats.fields_added += 1;
                }
                Some(existing) if existing == incoming => {} // identical — no-op
                Some(existing) => {
                    let conflict = FieldConflict {
                        column_id: target_id.clone(),
                        column_name: column_name.clone(),
                        field: key,
                        existing,
                        incoming,
                    };
                    stats.conflicts.push(conflict.clone());
                    match mode {
                        MergeMode::Plan => stats.unresolved.push(conflict),
                        MergeMode::Apply(resolution) => {
                            match resolution.choice_for(&target_id, key) {
                                Some(ConflictChoice::TakeIncoming) => {
                                    copy_field(&mut base, &entry.field, key);
                                    stats.resolved += 1;
                                }
                                Some(ConflictChoice::KeepExisting) => stats.resolved += 1,
                                None => stats.unresolved.push(conflict),
                            }
                        }
                    }
                }
            }
        }

        // Only store an entry that carries real documentation.
        if base.is_documented() {
            let changed = original.as_ref() != Some(&base);
            working.set(base);
            match original {
                None => stats.new_entries.push(target_id),
                Some(_) if changed => stats.updated_entries += 1,
                Some(_) => {}
            }
        }
    }

    (working, stats)
}

/// Dry-run a merge: report every conflict and what would change, without
/// touching the document.
pub fn plan_merge(
    doc: &Document,
    imported: &DictionaryExport,
    match_by: MergeMatchBy,
) -> MergePlan {
    let (_working, stats) = run_merge(doc, imported, match_by, &MergeMode::Plan);
    MergePlan {
        dictionary_revision: doc.dictionary_revision(),
        match_by,
        matched_columns: stats.matched,
        new_entries: stats.new_entries,
        clean_additions: stats.fields_added,
        conflicts: stats.conflicts,
        unmatched: stats.unmatched,
    }
}

/// Result of applying an import merge (the new dictionary + a summary).
pub struct MergeApplied {
    pub dictionary: Dictionary,
    pub matched_columns: usize,
    pub new_entries: usize,
    pub updated_entries: usize,
    pub fields_added: usize,
    pub conflicts_resolved: usize,
    pub unmatched: Vec<String>,
}

/// Apply an import merge under an explicit resolution. Fails (without changing
/// anything) when the resolution leaves any conflict unresolved.
pub fn apply_merge(
    doc: &Document,
    imported: &DictionaryExport,
    match_by: MergeMatchBy,
    resolution: &MergeResolution,
) -> AppResult<MergeApplied> {
    let (working, stats) = run_merge(doc, imported, match_by, &MergeMode::Apply(resolution));
    if !stats.unresolved.is_empty() {
        let first = &stats.unresolved[0];
        return Err(AppError::invalid(format!(
            "{} documentation conflict(s) still need explicit resolution (e.g. column \"{}\" field \"{}\")",
            stats.unresolved.len(),
            first.column_name,
            first.field.label()
        )));
    }
    Ok(MergeApplied {
        dictionary: working,
        matched_columns: stats.matched,
        new_entries: stats.new_entries.len(),
        updated_entries: stats.updated_entries,
        fields_added: stats.fields_added,
        conflicts_resolved: stats.resolved,
        unmatched: stats.unmatched,
    })
}

/// The serializable outcome returned to the front end after an import.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictionaryImportOutcome {
    pub matched_columns: usize,
    pub new_entries: usize,
    pub updated_entries: usize,
    pub fields_added: usize,
    pub conflicts_resolved: usize,
    pub unmatched: Vec<String>,
    pub view: DictionaryView,
}

// ---------------------------------------------------------------------------
// F08 profile hook: required-documentation rule + checker
// ---------------------------------------------------------------------------

/// A file-profile (F08) rule requiring certain dictionary fields to be
/// populated. `columns` names the technical columns it applies to; empty means
/// every column in the document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequiredDocumentation {
    #[serde(default)]
    pub columns: Vec<String>,
    pub fields: Vec<DictionaryFieldKey>,
}

/// One column missing required documentation.
pub struct DocumentationGap {
    pub column_id: String,
    pub column_name: String,
    pub missing: Vec<DictionaryFieldKey>,
}

/// Evaluate the required-documentation rules against the document's dictionary.
/// Returns one gap per (column, rule) that leaves a required field blank.
/// Columns a rule names that are not present are skipped (other profile rules
/// report missing columns).
pub fn documentation_gaps(
    doc: &Document,
    rules: &[RequiredDocumentation],
) -> Vec<DocumentationGap> {
    let mut gaps = Vec::new();
    for rule in rules {
        if rule.fields.is_empty() {
            continue;
        }
        // Resolve target column positions. A required-doc rule applies to EVERY
        // column sharing a named header, not just the first — headers are not
        // unique — and duplicate positions are collapsed so a column is reported
        // at most once per rule.
        let targets: Vec<usize> = if rule.columns.is_empty() {
            (0..doc.column_ids().len()).collect()
        } else {
            let mut positions: Vec<usize> = rule
                .columns
                .iter()
                .flat_map(|name| {
                    let name = name.trim();
                    doc.headers()
                        .iter()
                        .enumerate()
                        .filter(move |(_, h)| h.trim().eq_ignore_ascii_case(name))
                        .map(|(i, _)| i)
                })
                .collect();
            positions.sort_unstable();
            positions.dedup();
            positions
        };
        for pos in targets {
            let column_id = &doc.column_ids()[pos];
            let entry = doc.dictionary().field(column_id);
            let missing: Vec<DictionaryFieldKey> = rule
                .fields
                .iter()
                .copied()
                .filter(|&key| match entry {
                    Some(f) => !field_present(f, key),
                    None => true,
                })
                .collect();
            if !missing.is_empty() {
                gaps.push(DocumentationGap {
                    column_id: column_id.clone(),
                    column_name: doc.headers()[pos].clone(),
                    missing,
                });
            }
        }
    }
    gaps
}

// ---------------------------------------------------------------------------
// F28 PII hook: sensitive columns
// ---------------------------------------------------------------------------

/// A column the dictionary classifies as confidential or restricted.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SensitiveColumn {
    pub column: usize,
    pub column_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub sensitivity: Sensitivity,
}

/// Columns whose declared sensitivity makes them PII-relevant regardless of
/// pattern hits. Consumed by the F28 scan preflight. Ordered by column index.
pub fn sensitive_columns(doc: &Document) -> Vec<SensitiveColumn> {
    doc.column_ids()
        .iter()
        .enumerate()
        .filter_map(|(idx, id)| {
            let field = doc.dictionary().field(id)?;
            let sensitivity = field.sensitivity?;
            sensitivity.is_sensitive().then(|| SensitiveColumn {
                column: idx,
                column_id: id.clone(),
                display_name: field.display_name.clone().filter(|s| !s.trim().is_empty()),
                sensitivity,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse, ParseSettings};

    fn doc(csv: &str) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, true)
    }

    fn field(column_id: &str) -> DictionaryField {
        DictionaryField::empty(column_id)
    }

    fn export_of(entries: Vec<DictionaryExportEntry>) -> DictionaryExport {
        DictionaryExport {
            version: DICTIONARY_VERSION,
            entries,
        }
    }

    fn export_entry(
        column_id: &str,
        column_name: &str,
        f: impl FnOnce(&mut DictionaryField),
    ) -> DictionaryExportEntry {
        let mut fld = field(column_id);
        f(&mut fld);
        DictionaryExportEntry {
            column_name: column_name.to_string(),
            field: fld,
        }
    }

    // ----- storage: no-dirty, rename survival, orphans ---------------------

    #[test]
    fn dictionary_edits_never_dirty_or_move_the_document() {
        let mut d = doc("a,b\n1,2\n");
        let rev = d.revision();
        let dict_rev = d.dictionary_revision();
        assert!(!d.is_dirty());

        let mut f = field(&d.column_ids()[0].clone());
        f.description = Some("the primary key".into());
        d.set_dictionary_field(f);

        assert_eq!(d.revision(), rev, "document revision must not move");
        assert!(!d.is_dirty(), "documentation edits never dirty the source");
        assert_eq!(
            d.dictionary_revision(),
            dict_rev + 1,
            "the metadata revision moves instead"
        );
    }

    #[test]
    fn entry_survives_a_rename_by_stable_id() {
        let mut d = doc("email,amount\nx,1\n");
        let id = d.column_ids()[0].clone();
        let mut f = field(&id);
        f.description = Some("customer email".into());
        d.set_dictionary_field(f);

        d.rename_column(0, "contact_email".into()).unwrap();

        let entry = d.dictionary().field(&id).expect("entry preserved by ID");
        assert_eq!(entry.description.as_deref(), Some("customer email"));
        // The view now shows the NEW technical name against the same entry.
        let v = view(&d);
        assert_eq!(v.entries[0].column_name, "contact_email");
        assert!(v.entries[0].documented);
        assert!(orphans(&d).is_empty());
    }

    #[test]
    fn deleting_a_column_reports_an_orphan_and_keeps_the_entry() {
        let mut d = doc("a,b,c\n1,2,3\n");
        let id_b = d.column_ids()[1].clone();
        let mut f = field(&id_b);
        f.description = Some("the middle column".into());
        d.set_dictionary_field(f);

        d.delete_columns(vec![1]).unwrap();

        let orphs = orphans(&d);
        assert_eq!(orphs.len(), 1);
        assert_eq!(orphs[0].column_id, id_b);
        assert_eq!(
            orphs[0].field.description.as_deref(),
            Some("the middle column")
        );
        // The editor view no longer lists it as a live column.
        assert!(view(&d).entries.iter().all(|e| e.column_id != id_b));

        // Undo restores the column: the entry re-attaches (no longer orphaned).
        d.undo().unwrap();
        assert!(orphans(&d).is_empty());
        assert!(d.dictionary().field(&id_b).is_some());

        // Redo re-orphans it; discarding removes it explicitly.
        d.redo().unwrap();
        assert_eq!(orphans(&d).len(), 1);
        assert!(d.remove_dictionary_field(&id_b));
        assert!(orphans(&d).is_empty());
    }

    // ----- merge matrix: id / name / conflict ------------------------------

    #[test]
    fn merge_by_column_id_adds_and_flags_conflicts() {
        let mut d = doc("a,b\n1,2\n");
        let id_a = d.column_ids()[0].clone();
        // Existing docs on column a: description set, owner blank.
        let mut existing = field(&id_a);
        existing.description = Some("existing description".into());
        d.set_dictionary_field(existing);

        // Incoming: same column ID, a CONFLICTING description + a NEW owner.
        let incoming = export_of(vec![export_entry(&id_a, "a", |f| {
            f.description = Some("incoming description".into());
            f.owner = Some("data-team".into());
        })]);

        let plan = plan_merge(&d, &incoming, MergeMatchBy::ColumnId);
        assert_eq!(plan.matched_columns, 1);
        assert_eq!(plan.clean_additions, 1, "owner is a clean addition");
        assert_eq!(plan.conflicts.len(), 1, "description conflicts");
        assert_eq!(plan.conflicts[0].field, DictionaryFieldKey::Description);
        assert_eq!(plan.conflicts[0].existing, "existing description");
        assert_eq!(plan.conflicts[0].incoming, "incoming description");
        assert!(plan.unmatched.is_empty());

        // Applying without resolving the conflict is impossible under PerField.
        let unresolved = apply_merge(
            &d,
            &incoming,
            MergeMatchBy::ColumnId,
            &MergeResolution::PerField {
                resolutions: vec![],
            },
        );
        assert!(unresolved.is_err(), "unresolved conflicts block the apply");

        // Resolve by taking the incoming value; owner still merges cleanly.
        let applied = apply_merge(
            &d,
            &incoming,
            MergeMatchBy::ColumnId,
            &MergeResolution::PerField {
                resolutions: vec![FieldResolution {
                    column_id: id_a.clone(),
                    field: DictionaryFieldKey::Description,
                    choice: ConflictChoice::TakeIncoming,
                }],
            },
        )
        .unwrap();
        assert_eq!(applied.conflicts_resolved, 1);
        assert_eq!(applied.fields_added, 1);
        let merged = applied.dictionary.field(&id_a).unwrap();
        assert_eq!(merged.description.as_deref(), Some("incoming description"));
        assert_eq!(merged.owner.as_deref(), Some("data-team"));
    }

    #[test]
    fn keep_existing_resolution_preserves_current_values() {
        let mut d = doc("a\n1\n");
        let id_a = d.column_ids()[0].clone();
        let mut existing = field(&id_a);
        existing.description = Some("keep me".into());
        d.set_dictionary_field(existing);

        let incoming = export_of(vec![export_entry(&id_a, "a", |f| {
            f.description = Some("overwrite me".into());
        })]);
        let applied = apply_merge(
            &d,
            &incoming,
            MergeMatchBy::ColumnId,
            &MergeResolution::KeepAllExisting,
        )
        .unwrap();
        assert_eq!(
            applied
                .dictionary
                .field(&id_a)
                .unwrap()
                .description
                .as_deref(),
            Some("keep me")
        );
    }

    #[test]
    fn merge_by_mapped_name_when_ids_differ() {
        // The document's IDs are c0/c1; the import carries foreign IDs but
        // matching column NAMES.
        let d = doc("email,amount\nx,1\n");
        let incoming = export_of(vec![
            export_entry("foreign-99", "email", |f| {
                f.description = Some("the email".into());
            }),
            export_entry("foreign-100", "amount", |f| {
                f.owner = Some("finance".into());
            }),
        ]);

        // By ID nothing matches; by name (or Auto) both do.
        let by_id = plan_merge(&d, &incoming, MergeMatchBy::ColumnId);
        assert_eq!(by_id.matched_columns, 0);
        assert_eq!(by_id.unmatched.len(), 2);

        let applied = apply_merge(
            &d,
            &incoming,
            MergeMatchBy::ColumnName,
            &MergeResolution::KeepAllExisting,
        )
        .unwrap();
        assert_eq!(applied.matched_columns, 2);
        assert_eq!(applied.new_entries, 2);
        // Entries land under the DOCUMENT's IDs, not the foreign ones.
        assert_eq!(
            applied
                .dictionary
                .field(&d.column_ids()[0])
                .unwrap()
                .description
                .as_deref(),
            Some("the email")
        );
        assert!(applied.dictionary.field("foreign-99").is_none());
    }

    #[test]
    fn auto_prefers_id_then_falls_back_to_name() {
        let d = doc("a,b\n1,2\n");
        let id_a = d.column_ids()[0].clone();
        let incoming = export_of(vec![
            // Matches column a by its real ID.
            export_entry(&id_a, "renamed-header", |f| f.unit = Some("USD".into())),
            // No such ID; matches column b by name.
            export_entry("nope", "b", |f| f.unit = Some("kg".into())),
        ]);
        let plan = plan_merge(&d, &incoming, MergeMatchBy::Auto);
        assert_eq!(plan.matched_columns, 2);
        assert!(plan.unmatched.is_empty());
    }

    #[test]
    fn merge_by_name_reports_ambiguous_duplicate_headers() {
        // Headers are not unique — two columns share the name "email". An
        // import entry that matches by name cannot be safely attributed to
        // either, so it is reported as unmatched, NOT silently collapsed onto
        // the first column.
        let d = doc("email,email\n1,2\n");
        let incoming = export_of(vec![export_entry("foreign-1", "email", |f| {
            f.description = Some("the email".into());
        })]);

        let plan = plan_merge(&d, &incoming, MergeMatchBy::ColumnName);
        assert_eq!(plan.matched_columns, 0, "ambiguous name matches nothing");
        assert!(plan.new_entries.is_empty());
        assert_eq!(plan.unmatched.len(), 1);
        assert!(
            plan.unmatched[0].contains("ambiguous"),
            "unmatched entry carries a reason: {:?}",
            plan.unmatched[0]
        );

        // Applying attributes documentation to NEITHER column.
        let applied = apply_merge(
            &d,
            &incoming,
            MergeMatchBy::ColumnName,
            &MergeResolution::KeepAllExisting,
        )
        .unwrap();
        assert_eq!(applied.matched_columns, 0);
        assert_eq!(applied.new_entries, 0);
        assert!(applied.dictionary.field(&d.column_ids()[0]).is_none());
        assert!(applied.dictionary.field(&d.column_ids()[1]).is_none());
        assert_eq!(applied.unmatched.len(), 1);
    }

    #[test]
    fn case_variant_headers_are_also_ambiguous_by_name() {
        // "Email" and "email" collide under case-insensitive name matching.
        let d = doc("Email,email\n1,2\n");
        let incoming = export_of(vec![export_entry("foreign-1", "EMAIL", |f| {
            f.owner = Some("data".into());
        })]);
        let plan = plan_merge(&d, &incoming, MergeMatchBy::ColumnName);
        assert_eq!(plan.matched_columns, 0);
        assert_eq!(plan.unmatched.len(), 1);
    }

    #[test]
    fn auto_uses_id_even_when_the_name_is_ambiguous() {
        // Duplicate headers, but the import carries the real ID of the SECOND
        // column — Auto prefers the (unambiguous) ID and lands there exactly.
        let d = doc("email,email\n1,2\n");
        let id1 = d.column_ids()[1].clone();
        let incoming = export_of(vec![export_entry(&id1, "email", |f| {
            f.owner = Some("finance".into());
        })]);
        let applied = apply_merge(
            &d,
            &incoming,
            MergeMatchBy::Auto,
            &MergeResolution::KeepAllExisting,
        )
        .unwrap();
        assert_eq!(applied.matched_columns, 1);
        assert_eq!(
            applied.dictionary.field(&id1).unwrap().owner.as_deref(),
            Some("finance")
        );
        // The first same-named column is untouched.
        assert!(applied.dictionary.field(&d.column_ids()[0]).is_none());
    }

    #[test]
    fn identical_incoming_value_is_not_a_conflict() {
        let mut d = doc("a\n1\n");
        let id_a = d.column_ids()[0].clone();
        let mut existing = field(&id_a);
        existing.description = Some("same".into());
        d.set_dictionary_field(existing);
        let incoming = export_of(vec![export_entry(&id_a, "a", |f| {
            f.description = Some("same".into());
        })]);
        let plan = plan_merge(&d, &incoming, MergeMatchBy::ColumnId);
        assert!(plan.conflicts.is_empty());
        assert_eq!(plan.clean_additions, 0);
    }

    // ----- export completeness: JSON round-trip, MD, CSV -------------------

    fn fully_documented(d: &mut Document) -> String {
        let id = d.column_ids()[0].clone();
        let mut f = field(&id);
        f.display_name = Some("Customer Email".into());
        f.description = Some("primary contact email".into());
        f.role = Some(FieldRole::Dimension);
        f.unit = Some("n/a".into());
        f.source = Some("CRM".into());
        f.sensitivity = Some(Sensitivity::Confidential);
        f.allowed_values = vec!["a@x.com".into(), "b@x.com".into()];
        f.example = Some("a@x.com".into());
        f.owner = Some("data-team".into());
        f.notes = Some("deduplicated nightly".into());
        d.set_dictionary_field(f);
        id
    }

    #[test]
    fn markdown_export_contains_every_documented_field() {
        let mut d = doc("email,amount\nx,1\n");
        fully_documented(&mut d);
        let md = export_markdown(&d);
        for needle in [
            "Customer Email",
            "primary contact email",
            "dimension",
            "CRM",
            "confidential",
            "a@x.com, b@x.com",
            "data-team",
            "deduplicated nightly",
        ] {
            assert!(md.contains(needle), "markdown missing {needle:?}:\n{md}");
        }
    }

    #[test]
    fn csv_export_contains_every_documented_field() {
        let mut d = doc("email,amount\nx,1\n");
        fully_documented(&mut d);
        let csv = export_csv(&d).unwrap();
        // Header row + one data row.
        let mut reader = csv::ReaderBuilder::new().from_reader(csv.as_bytes());
        let headers = reader.headers().unwrap().clone();
        assert!(headers.iter().any(|h| h == "sensitivity"));
        let row = reader.records().next().unwrap().unwrap();
        let get = |name: &str| {
            let i = headers.iter().position(|h| h == name).unwrap();
            row.get(i).unwrap().to_string()
        };
        assert_eq!(get("displayName"), "Customer Email");
        assert_eq!(get("description"), "primary contact email");
        assert_eq!(get("role"), "dimension");
        assert_eq!(get("source"), "CRM");
        assert_eq!(get("sensitivity"), "confidential");
        assert_eq!(get("allowedValues"), "a@x.com; b@x.com");
        assert_eq!(get("owner"), "data-team");
        assert_eq!(get("notes"), "deduplicated nightly");
    }

    #[test]
    fn json_export_round_trips_and_reimports_by_id() {
        let mut d = doc("email,amount\nx,1\n");
        let id = fully_documented(&mut d);
        let json = export_json(&d).unwrap();
        let parsed = parse_import(&json).unwrap();
        assert_eq!(parsed.version, DICTIONARY_VERSION);
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].column_name, "email");
        assert_eq!(parsed.entries[0].field.column_id, id);
        assert_eq!(
            parsed.entries[0].field.sensitivity,
            Some(Sensitivity::Confidential)
        );

        // Re-importing onto a blank document reproduces the entry.
        let blank = doc("email,amount\nx,1\n");
        let applied = apply_merge(
            &blank,
            &parsed,
            MergeMatchBy::Auto,
            &MergeResolution::KeepAllExisting,
        )
        .unwrap();
        assert_eq!(applied.new_entries, 1);
        assert_eq!(
            applied
                .dictionary
                .field(&blank.column_ids()[0])
                .unwrap()
                .description
                .as_deref(),
            Some("primary contact email")
        );
    }

    #[test]
    fn parse_import_rejects_unknown_version() {
        let json = format!(
            r#"{{"version": {}, "entries": []}}"#,
            DICTIONARY_VERSION + 1
        );
        assert!(parse_import(&json).is_err());
    }

    // ----- profile hook ----------------------------------------------------

    #[test]
    fn documentation_gaps_flag_missing_required_fields() {
        let mut d = doc("a,b\n1,2\n");
        // Column a documented with a description but no owner; b undocumented.
        let mut f = field(&d.column_ids()[0].clone());
        f.description = Some("has a description".into());
        d.set_dictionary_field(f);

        let rules = vec![RequiredDocumentation {
            columns: vec![], // all columns
            fields: vec![DictionaryFieldKey::Description, DictionaryFieldKey::Owner],
        }];
        let gaps = documentation_gaps(&d, &rules);
        // a is missing owner; b is missing both.
        assert_eq!(gaps.len(), 2);
        let a = gaps.iter().find(|g| g.column_name == "a").unwrap();
        assert_eq!(a.missing, vec![DictionaryFieldKey::Owner]);
        let b = gaps.iter().find(|g| g.column_name == "b").unwrap();
        assert_eq!(
            b.missing,
            vec![DictionaryFieldKey::Description, DictionaryFieldKey::Owner]
        );

        // Scoping the rule to a specific column limits the gaps.
        let scoped = vec![RequiredDocumentation {
            columns: vec!["a".into()],
            fields: vec![DictionaryFieldKey::Description],
        }];
        assert!(
            documentation_gaps(&d, &scoped).is_empty(),
            "a has a description"
        );
    }

    #[test]
    fn documentation_gaps_cover_every_column_sharing_a_name() {
        // Two columns share the header "email"; a rule naming "email" must flag
        // BOTH (not just the first) and report each at most once.
        let d = doc("email,email\n1,2\n");
        let rules = vec![RequiredDocumentation {
            columns: vec!["email".into()],
            fields: vec![DictionaryFieldKey::Description],
        }];
        let gaps = documentation_gaps(&d, &rules);
        assert_eq!(gaps.len(), 2, "both same-named columns are flagged");
        let ids: HashSet<&str> = gaps.iter().map(|g| g.column_id.as_str()).collect();
        assert!(ids.contains(d.column_ids()[0].as_str()));
        assert!(ids.contains(d.column_ids()[1].as_str()));
    }

    // ----- PII hook --------------------------------------------------------

    #[test]
    fn sensitive_columns_selects_confidential_and_restricted() {
        let mut d = doc("public_col,secret_col,top_col\n1,2,3\n");
        let ids: Vec<String> = d.column_ids().to_vec();
        let mut p = field(&ids[0]);
        p.sensitivity = Some(Sensitivity::Public);
        d.set_dictionary_field(p);
        let mut s = field(&ids[1]);
        s.sensitivity = Some(Sensitivity::Confidential);
        s.display_name = Some("Secret".into());
        d.set_dictionary_field(s);
        let mut t = field(&ids[2]);
        t.sensitivity = Some(Sensitivity::Restricted);
        d.set_dictionary_field(t);

        let sensitive = sensitive_columns(&d);
        assert_eq!(sensitive.len(), 2, "public is not flagged");
        assert_eq!(sensitive[0].column, 1);
        assert_eq!(sensitive[0].sensitivity, Sensitivity::Confidential);
        assert_eq!(sensitive[0].display_name.as_deref(), Some("Secret"));
        assert_eq!(sensitive[1].column, 2);
        assert_eq!(sensitive[1].sensitivity, Sensitivity::Restricted);
    }

    #[test]
    fn empty_field_is_not_stored_as_documented() {
        let d = doc("a\n1\n");
        let id = d.column_ids()[0].clone();
        assert!(!field(&id).is_documented());
        let v = view(&d);
        assert!(!v.entries[0].documented);
        assert_eq!(v.entries[0].field.column_id, id);
    }
}
