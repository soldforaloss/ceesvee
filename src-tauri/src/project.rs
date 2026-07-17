//! F37 project workspaces: a complete local working context across related
//! datasets, persisted in a versioned `.ceesveeproj` JSON file.
//!
//! [`ProjectStore`] is THE persistence boundary for workspace state. The file
//! is a versioned envelope `{ formatVersion, appVersion, sections }` whose
//! sections are typed and registered in [`SECTION_REGISTRY`]. Future features
//! extend the registry by adding a typed field to [`ProjectSections`], a row
//! to the registry, and a match arm in [`set_section_typed`] — the reserved
//! `annotations` (F40), `dictionary` (F38) and `queries` (F36) sections are
//! already named, default-empty, and rejected for writes until their owning
//! feature lands.
//!
//! Hard rules enforced here:
//!
//! - **No source data.** A project references documents; it never embeds cell
//!   values. Every section write and every save scans the serialized JSON and
//!   rejects data-bearing keys ([`FORBIDDEN_SECTION_KEYS`]), so the store can
//!   never quietly become a database of copied user data.
//! - **Relative paths when possible.** Source paths are stored relative to
//!   the project file (forward slashes), falling back to absolute across
//!   drive roots. In memory and over IPC they are always absolute; encoding
//!   happens at write time, decoding at load time.
//! - **Atomic save** through the F03 staging + fsync + rename pipeline; a
//!   failed save leaves the previous file byte-for-byte intact.
//! - **Safe open.** The file is fully parsed before any state changes; a
//!   corrupt file errors clearly and is never modified or moved aside. Files
//!   written by a newer MAJOR format are rejected with a clear message; a
//!   newer minor is accepted, and the unknown fields it adds at the envelope,
//!   section-map, source, tabs and per-source open-settings levels are
//!   preserved on round-trip via serde flatten catch-alls, alongside the
//!   version string. Typed sub-sections owned by other features (views,
//!   schemas, comparisons, row keys) round-trip through those features' own
//!   versioned payloads rather than through a catch-all here.
//! - **Nothing runs on open.** Opening a project yields a [`ProjectOpenPlan`]
//!   describing what to open and which named views are safe to reapply
//!   (fingerprint + column compatibility gated — warn, never break). Recipes,
//!   queries, joins, comparisons and exports are surfaced as configuration
//!   only; the front end must never execute them as a side effect of open.

use std::collections::{BTreeMap, HashMap};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::State;

use crate::compare::CompareSpec;
use crate::dto::{BackupPolicy, FileFingerprint};
use crate::error::{AppError, AppResult};
use crate::row_identity::KeySpec;
use crate::save;
use crate::schema::SchemaExport;
use crate::settings::{FileProfile, NamedView, ProfileMatch};
use crate::{encoding, util};

/// Major format version this build reads and writes. Files with a HIGHER
/// major are rejected (a lower or equal major with a newer minor is fine).
pub const PROJECT_FORMAT_MAJOR: u32 = 1;
/// Minor format version this build writes.
pub const PROJECT_FORMAT_MINOR: u32 = 0;
/// Canonical project file extension (not enforced on open; the front end's
/// save/open dialogs filter on it).
#[allow(dead_code)]
pub const PROJECT_EXTENSION: &str = "ceesveeproj";

/// JSON object keys that may never appear anywhere inside a project's
/// sections: they are the names cell/row data would serialize under. This is
/// the structural enforcement of "a project references data, it never copies
/// it". Future sections must pick different key names for configuration
/// (e.g. a dictionary's enumeration should be `allowedValues`, not `values`).
pub const FORBIDDEN_SECTION_KEYS: &[&str] = &[
    "cells",
    "cellValues",
    "cellData",
    "rows",
    "rowData",
    "records",
    "values",
    "samples",
    "sampleRows",
    "clipboard",
];

fn current_format_version() -> String {
    format!("{PROJECT_FORMAT_MAJOR}.{PROJECT_FORMAT_MINOR}")
}

fn app_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

// ---------------------------------------------------------------------------
// File model (envelope + typed sections)
// ---------------------------------------------------------------------------

/// The on-disk envelope. Unknown top-level fields written by future versions
/// are preserved verbatim through `unknown`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectFile {
    /// "major.minor". Kept verbatim from the loaded file (so a 1.9 file
    /// saved by a 1.0 build stays 1.9, matching its preserved fields).
    pub format_version: String,
    /// CEESVEE version that last wrote the file (informational).
    pub app_version: String,
    /// Template files carry configuration without sources; they initialize
    /// new projects and cannot be opened as projects.
    #[serde(default)]
    pub template: bool,
    #[serde(default)]
    pub sections: ProjectSections,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

impl ProjectFile {
    fn new_empty() -> ProjectFile {
        ProjectFile {
            format_version: current_format_version(),
            app_version: app_version(),
            template: false,
            sections: ProjectSections::default(),
            unknown: BTreeMap::new(),
        }
    }
}

/// The typed, registered sections. Every field is optional-with-default so
/// older files load cleanly; unknown sections written by future versions are
/// preserved verbatim through `unknown`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSections {
    /// The referenced documents (paths absolute in memory, relativized on
    /// disk) with their identity fingerprints and parse settings.
    #[serde(default)]
    pub sources: Vec<ProjectSource>,
    /// Open-tab order and the active tab, by source id.
    #[serde(default)]
    pub tabs: TabsSection,
    /// Front-end-owned panel layout, round-tripped verbatim (still subject
    /// to the no-cell-data scan).
    #[serde(default)]
    pub layout: Option<Value>,
    /// F12 named views, per source.
    #[serde(default)]
    pub views: Vec<SourceViews>,
    /// F08 file profiles bundled with the project.
    #[serde(default)]
    pub profiles: Vec<FileProfile>,
    /// F31 schemas in their versioned export form, per source.
    #[serde(default)]
    pub schemas: Vec<SourceSchema>,
    /// F25 recipes (opaque recipe JSON, validated by the recipe engine when
    /// — and only when — the user explicitly runs one).
    #[serde(default)]
    pub recipes: Vec<NamedConfig>,
    /// Join mappings between two sources (opaque join-spec JSON, validated
    /// by the join engine on explicit use).
    #[serde(default)]
    pub join_mappings: Vec<JoinMapping>,
    /// F09 comparison definitions between two sources.
    #[serde(default)]
    pub comparisons: Vec<ComparisonDef>,
    /// Row-identity key definitions (shared `row_identity::KeySpec`), per
    /// source.
    #[serde(default)]
    pub row_keys: Vec<SourceRowKey>,
    /// RESERVED for F40 row annotations: entries will reference rows by
    /// `row_identity::RowIdentity` (keys/record numbers), never by content.
    #[serde(default)]
    pub annotations: Vec<Value>,
    /// RESERVED for F38 data dictionaries: per-column descriptions and
    /// constraints (configuration only).
    #[serde(default)]
    pub dictionary: Vec<Value>,
    /// RESERVED for F36 saved queries: definitions only, never results.
    #[serde(default)]
    pub queries: Vec<Value>,
    /// Sections written by future versions, preserved verbatim.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

/// One entry in the section registry.
pub struct SectionSpec {
    pub name: &'static str,
    /// Reserved sections are named and preserved but reject writes until
    /// their owning feature registers real types.
    pub reserved: bool,
    /// The feature that owns the section's schema.
    pub owner: &'static str,
}

/// The registration table future features extend (add a field to
/// [`ProjectSections`], a row here, and an arm in [`set_section_typed`]).
pub const SECTION_REGISTRY: &[SectionSpec] = &[
    SectionSpec {
        name: "sources",
        reserved: false,
        owner: "F37",
    },
    SectionSpec {
        name: "tabs",
        reserved: false,
        owner: "F37",
    },
    SectionSpec {
        name: "layout",
        reserved: false,
        owner: "F37",
    },
    SectionSpec {
        name: "views",
        reserved: false,
        owner: "F12",
    },
    SectionSpec {
        name: "profiles",
        reserved: false,
        owner: "F08",
    },
    SectionSpec {
        name: "schemas",
        reserved: false,
        owner: "F31",
    },
    SectionSpec {
        name: "recipes",
        reserved: false,
        owner: "F25",
    },
    SectionSpec {
        name: "joinMappings",
        reserved: false,
        owner: "F24",
    },
    SectionSpec {
        name: "comparisons",
        reserved: false,
        owner: "F09",
    },
    SectionSpec {
        name: "rowKeys",
        reserved: false,
        owner: "F37",
    },
    SectionSpec {
        name: "annotations",
        reserved: true,
        owner: "F40",
    },
    SectionSpec {
        name: "dictionary",
        reserved: true,
        owner: "F38",
    },
    SectionSpec {
        name: "queries",
        reserved: true,
        owner: "F36",
    },
];

/// Column snapshot captured at save time (id + header text), used on open to
/// cheaply verify that a changed file is still column-compatible with the
/// saved views/schemas before reapplying them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectColumn {
    pub id: String,
    pub name: String,
}

/// Parse settings a source was loaded with (mirrors the effective values in
/// `DocumentMeta`, so reopening reproduces the same interpretation).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ProjectOpenSettings {
    pub delimiter: Option<String>,
    pub encoding: Option<String>,
    pub has_header_row: Option<bool>,
    /// Fields a future minor adds to a source's open settings, preserved
    /// verbatim on round-trip.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

/// One referenced document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSource {
    /// Project-stable id other sections reference (never a path).
    pub id: String,
    /// Absolute in memory / over IPC; relativized against the project file
    /// on disk when possible.
    pub path: String,
    #[serde(default)]
    pub display_name: Option<String>,
    /// Identity of the file the project's state was captured against.
    #[serde(default)]
    pub fingerprint: Option<FileFingerprint>,
    #[serde(default)]
    pub open: ProjectOpenSettings,
    /// Column snapshot for compatibility checks (metadata only).
    #[serde(default)]
    pub columns: Vec<ProjectColumn>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

/// Open-tab order and active tab, by source id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TabsSection {
    pub open: Vec<String>,
    pub active: Option<String>,
    /// Fields a future minor adds to the tabs section, preserved verbatim on
    /// round-trip.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

/// F12 named views attached to one source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceViews {
    pub source_id: String,
    #[serde(default)]
    pub views: Vec<NamedView>,
    #[serde(default)]
    pub active_view_id: Option<String>,
}

/// F31 schema for one source, in the versioned export envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSchema {
    pub source_id: String,
    pub schema: SchemaExport,
}

/// Row-identity key definition for one source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRowKey {
    pub source_id: String,
    pub key: KeySpec,
}

/// A named opaque configuration payload (used for recipes, whose engine
/// types are deserialize-only and validate on explicit use).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NamedConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub config: Value,
}

/// A saved join mapping between two sources.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinMapping {
    pub id: String,
    pub name: String,
    pub left_source_id: String,
    pub right_source_id: String,
    /// `joins::JoinSpec` JSON, validated by the join engine on explicit use.
    #[serde(default)]
    pub spec: Value,
}

/// A saved comparison definition between two sources.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComparisonDef {
    pub id: String,
    pub name: String,
    pub left_source_id: String,
    pub right_source_id: String,
    pub spec: CompareSpec,
}

// ---------------------------------------------------------------------------
// No-cell-data enforcement
// ---------------------------------------------------------------------------

/// Recursively reject any JSON object key in [`FORBIDDEN_SECTION_KEYS`].
fn scan_for_data_keys(context: &str, value: &Value) -> AppResult<()> {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if FORBIDDEN_SECTION_KEYS.contains(&key.as_str()) {
                    return Err(AppError::invalid(format!(
                        "project section \"{context}\" contains the data-bearing key \
                         \"{key}\"; projects reference data, they never embed it"
                    )));
                }
                scan_for_data_keys(context, child)?;
            }
            Ok(())
        }
        Value::Array(items) => {
            for item in items {
                scan_for_data_keys(context, item)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Serialize `sections` and enforce the no-cell-data rule over the whole
/// tree (typed, reserved and unknown alike).
fn assert_sections_config_only(sections: &ProjectSections) -> AppResult<()> {
    let value = serde_json::to_value(sections)
        .map_err(|e| AppError::Other(format!("project serialization failed: {e}")))?;
    if let Value::Object(map) = &value {
        for (name, section) in map {
            scan_for_data_keys(name, section)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Version handling, parse, save
// ---------------------------------------------------------------------------

/// Parse the major component of a "major.minor" format version.
fn format_major(version: &str) -> AppResult<u32> {
    version
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok())
        .ok_or_else(|| {
            AppError::invalid(format!(
                "\"{version}\" is not a valid project format version (expected \"major.minor\")"
            ))
        })
}

/// Parse project bytes: probe the format version FIRST so a newer-major file
/// fails with the version message, not a shape error; then fully parse.
/// Never touches any state — a corrupt file is simply reported.
fn parse_project_bytes(bytes: &[u8]) -> AppResult<ProjectFile> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct VersionProbe {
        format_version: String,
    }
    let probe: VersionProbe = serde_json::from_slice(bytes)
        .map_err(|e| AppError::invalid(format!("this is not a valid CEESVEE project file: {e}")))?;
    let major = format_major(&probe.format_version)?;
    if major > PROJECT_FORMAT_MAJOR {
        return Err(AppError::invalid(format!(
            "this project was saved by a newer version of CEESVEE (format \
             {}, this build reads up to {PROJECT_FORMAT_MAJOR}.x); update \
             CEESVEE to open it — the file has not been modified",
            probe.format_version
        )));
    }
    serde_json::from_slice(bytes)
        .map_err(|e| AppError::invalid(format!("this is not a valid CEESVEE project file: {e}")))
}

/// Load a project file and decode its source paths to absolute (against the
/// project file's directory). Read-only: a corrupt or newer-major file
/// errors clearly and is left untouched.
pub(crate) fn load_project_file(path: &Path) -> AppResult<ProjectFile> {
    let bytes = std::fs::read(path).map_err(|e| {
        AppError::invalid(format!(
            "could not read project file {}: {e}",
            path.display()
        ))
    })?;
    let mut file = parse_project_bytes(&bytes)?;
    let dir = project_dir(path)?;
    for source in &mut file.sections.sources {
        source.path = decode_path(&dir, &source.path)
            .to_string_lossy()
            .to_string();
    }
    Ok(file)
}

/// Atomically write `file` to `path`, relativizing source paths against the
/// project directory and enforcing the no-cell-data rule first. A failure at
/// any point leaves the previous file byte-for-byte intact.
pub(crate) fn write_project_file(path: &Path, file: &ProjectFile) -> AppResult<()> {
    assert_sections_config_only(&file.sections)?;
    let dir = project_dir(path)?;
    let mut on_disk = file.clone();
    for source in &mut on_disk.sections.sources {
        source.path = encode_path(&dir, Path::new(&source.path));
    }
    let json = serde_json::to_vec_pretty(&on_disk)
        .map_err(|e| AppError::Other(format!("project serialization failed: {e}")))?;
    save::atomic_write(path, BackupPolicy::None, |f| {
        use std::io::Write;
        f.write_all(&json)?;
        Ok(json.len() as u64)
    })?;
    Ok(())
}

fn project_dir(path: &Path) -> AppResult<PathBuf> {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .ok_or_else(|| AppError::invalid("the project path has no parent directory"))
}

// ---------------------------------------------------------------------------
// Relative-path encode/decode
// ---------------------------------------------------------------------------

/// Express `target` relative to `base` (both absolute), walking up with
/// `..` where needed. `None` when the roots differ (e.g. different Windows
/// drives) — callers then fall back to the absolute path.
pub(crate) fn make_relative(base: &Path, target: &Path) -> Option<PathBuf> {
    if !base.is_absolute() || !target.is_absolute() {
        return None;
    }
    let base_parts: Vec<Component> = base.components().collect();
    let target_parts: Vec<Component> = target.components().collect();

    // Windows drive prefixes compare case-insensitively (`c:` == `C:`);
    // everything else compares exactly.
    let same = |a: &Component, b: &Component| match (a, b) {
        (Component::Prefix(pa), Component::Prefix(pb)) => pa
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&pb.as_os_str().to_string_lossy()),
        _ => a == b,
    };

    // The filesystem root (prefix + root dir) must match to relativize at all.
    if !same(base_parts.first()?, target_parts.first()?) {
        return None;
    }

    let common = base_parts
        .iter()
        .zip(target_parts.iter())
        .take_while(|(a, b)| same(a, b))
        .count();
    let mut rel = PathBuf::new();
    for _ in common..base_parts.len() {
        rel.push("..");
    }
    for part in &target_parts[common..] {
        rel.push(part.as_os_str());
    }
    if rel.as_os_str().is_empty() {
        rel.push(".");
    }
    Some(rel)
}

/// Encode an absolute source path for storage: relative to the project
/// directory with forward slashes when possible, absolute otherwise.
pub(crate) fn encode_path(dir: &Path, target: &Path) -> String {
    match make_relative(dir, target) {
        Some(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/"),
        None => target.to_string_lossy().to_string(),
    }
}

/// Decode a stored path: absolute paths pass through; relative ones are
/// joined to the project directory component by component (so stored
/// forward slashes become native separators) with `.`/`..` resolved
/// lexically.
pub(crate) fn decode_path(dir: &Path, stored: &str) -> PathBuf {
    let p = Path::new(stored);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    let mut out = dir.to_path_buf();
    for part in p.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Open flow: per-source status + resolutions + plan
// ---------------------------------------------------------------------------

/// Per-source condition found while previewing a project open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SourceStatus {
    /// File present and byte-identical to what the project captured.
    Ok,
    /// File absent and no replacement candidate found.
    Missing,
    /// File absent, but a same-named, size-matching file was found near the
    /// project — offered as a relink candidate.
    MovedCandidate,
    /// File present but changed since the project was saved (columns still
    /// look compatible); views are gated with a warning.
    ChangedFingerprint,
    /// File present but its columns no longer match the saved snapshot;
    /// views/schemas would not apply cleanly and are gated.
    SchemaIncompatible,
}

/// One source's preview line: status, fingerprints, gating verdict.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePreviewEntry {
    pub source_id: String,
    pub display_name: Option<String>,
    /// Absolute path the stored (possibly relative) path resolves to.
    pub resolved_path: String,
    pub status: SourceStatus,
    pub stored_fingerprint: Option<FileFingerprint>,
    pub disk_fingerprint: Option<FileFingerprint>,
    /// Absolute path of the relink candidate (MovedCandidate only).
    pub moved_candidate: Option<String>,
    /// Whether saved named views are safe to reapply (fingerprint match +
    /// column compatibility). Warn-never-break: a gated source still opens.
    pub reapply_views: bool,
    pub warnings: Vec<String>,
}

/// Everything the front end needs to render the open dialog. Produced
/// without touching any state.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectOpenPreview {
    pub path: String,
    pub format_version: String,
    pub app_version: String,
    pub sources: Vec<SourcePreviewEntry>,
    pub tab_order: Vec<String>,
    pub active_tab: Option<String>,
}

/// The user's per-source choice. Cancelling the whole open is simply not
/// calling `project_open_apply`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(
    tag = "action",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SourceResolution {
    /// Open from the stored path (the default; invalid for missing files).
    Open,
    /// Relink to a replacement file and open it.
    Locate { path: String },
    /// Keep the source in the project but do not open it this time
    /// ("open available only").
    Skip,
    /// Remove the source (and everything referencing it) from the project.
    Remove,
}

/// One resolution, addressed by source id.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolutionEntry {
    pub source_id: String,
    #[serde(flatten)]
    pub action: SourceResolution,
}

/// One document the front end should open (via the normal `open_file`
/// path), with the named views to reapply afterwards. The front end reapplies
/// each source's active view once its document has actually opened, but ONLY
/// when `reapply_views` is set (fingerprint + column compatibility held).
/// Views are carried even when gated so the warning banner can explain what
/// was skipped. Schemas and row-key definitions are NOT part of the open plan:
/// they stay in the persisted `schemas`/`rowKeys` sections and are consumed by
/// their owning features on demand, never reapplied as a side effect of open.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanEntry {
    pub source_id: String,
    /// Absolute path to open.
    pub path: String,
    pub display_name: Option<String>,
    pub open: ProjectOpenSettings,
    pub status: SourceStatus,
    pub reapply_views: bool,
    pub view_warnings: Vec<String>,
    pub views: Vec<NamedView>,
    pub active_view_id: Option<String>,
}

/// The resolved open plan. NOTHING here executes: the front end drives each
/// entry through the ordinary open pipeline and reapplies only what the
/// gating allows. Recipes/queries/joins stay stored configuration.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectOpenPlan {
    pub meta: ProjectMeta,
    /// In restored tab order.
    pub entries: Vec<PlanEntry>,
    pub tab_order: Vec<String>,
    pub active_tab: Option<String>,
    pub removed_source_ids: Vec<String>,
    pub skipped_source_ids: Vec<String>,
}

/// Outcome of the cheap column-compatibility check on a changed file.
enum ColumnCheck {
    Compatible,
    Incompatible(String),
    Unverifiable(String),
}

/// Read the first record of `path` (bounded) under the stored settings.
fn sniff_first_record(path: &Path, open: &ProjectOpenSettings) -> Option<Vec<String>> {
    use std::io::Read;
    const SNIFF_BYTES: u64 = 128 * 1024;
    let file = std::fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(SNIFF_BYTES).read_to_end(&mut bytes).ok()?;
    let enc = match &open.encoding {
        Some(name) => encoding::from_name(name),
        None => encoding::detect(&bytes).0,
    };
    let (text, _) = encoding::decode(&bytes, enc);
    let delimiter = open
        .delimiter
        .as_deref()
        .map(util::delimiter_to_byte)
        .unwrap_or(b',');
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut record = csv::StringRecord::new();
    match reader.read_record(&mut record) {
        Ok(true) => Some(record.iter().map(str::to_string).collect()),
        _ => None,
    }
}

/// Compare the saved column snapshot against the file's first record.
/// Positional name comparison, because stable column ids are positional at
/// parse — a renamed or reordered header means saved views/schemas would
/// bind to different columns.
fn column_check(path: &Path, source: &ProjectSource) -> ColumnCheck {
    if source.columns.is_empty() {
        return ColumnCheck::Unverifiable("no saved column snapshot to check against".into());
    }
    let Some(first) = sniff_first_record(path, &source.open) else {
        return ColumnCheck::Unverifiable("could not read the file to verify its columns".into());
    };
    let header_mode = match source.open.has_header_row {
        Some(explicit) => explicit,
        None => util::looks_like_header(&first),
    };
    if first.len() != source.columns.len() {
        return ColumnCheck::Incompatible(format!(
            "the file now has {} column(s); the project saved {}",
            first.len(),
            source.columns.len()
        ));
    }
    if header_mode {
        for (i, col) in source.columns.iter().enumerate() {
            let now = first[i].trim();
            if now != col.name {
                return ColumnCheck::Incompatible(format!(
                    "column {} is now \"{now}\" but the project saved \"{}\"",
                    i + 1,
                    col.name
                ));
            }
        }
    }
    ColumnCheck::Compatible
}

/// Look for a same-named, size-matching file in the project directory or its
/// immediate subdirectories (a bounded, cheap "did it just move?" search).
fn find_moved_candidate(dir: &Path, source: &ProjectSource, resolved: &Path) -> Option<PathBuf> {
    let name = resolved.file_name()?;
    let stored = source.fingerprint?;
    let mut dirs = vec![dir.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                dirs.push(p);
            }
        }
    }
    for d in dirs {
        let candidate = d.join(name);
        if candidate.as_path() != resolved {
            if let Some(fp) = util::stat_fingerprint(&candidate) {
                if fp.size == stored.size {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// Classify one source: status + view-reapply gating. Views reapply ONLY
/// when the stored fingerprint exists, matches the disk, and nothing
/// contradicts the column snapshot (a matching fingerprint means identical
/// bytes, so no further check is needed).
fn check_source(dir: &Path, source: &ProjectSource) -> SourcePreviewEntry {
    let resolved = decode_path(dir, &source.path);
    let disk = util::stat_fingerprint(&resolved);
    let mut warnings = Vec::new();
    let mut moved_candidate = None;

    let (status, reapply_views) = match (disk, source.fingerprint) {
        (None, _) => {
            warnings.push("the file is missing from its saved location".into());
            match find_moved_candidate(dir, source, &resolved) {
                Some(candidate) => {
                    warnings.push(format!(
                        "a same-named file was found at {}",
                        candidate.display()
                    ));
                    moved_candidate = Some(candidate.to_string_lossy().to_string());
                    (SourceStatus::MovedCandidate, false)
                }
                None => (SourceStatus::Missing, false),
            }
        }
        (Some(d), Some(s)) if d == s => (SourceStatus::Ok, true),
        (Some(_), stored) => match column_check(&resolved, source) {
            ColumnCheck::Incompatible(msg) => {
                warnings.push(format!(
                    "the file's columns changed since the project was saved ({msg}); \
                     saved views and schemas were not reapplied"
                ));
                (SourceStatus::SchemaIncompatible, false)
            }
            checked => {
                if let ColumnCheck::Unverifiable(reason) = &checked {
                    warnings.push(format!("could not verify column compatibility: {reason}"));
                }
                if stored.is_some() {
                    warnings.push(
                        "the file changed since the project was saved; saved views were \
                         not reapplied automatically"
                            .into(),
                    );
                    (SourceStatus::ChangedFingerprint, false)
                } else {
                    warnings.push(
                        "the project has no saved fingerprint for this file; saved views \
                         were not reapplied automatically"
                            .into(),
                    );
                    (SourceStatus::Ok, false)
                }
            }
        },
    };

    SourcePreviewEntry {
        source_id: source.id.clone(),
        display_name: source.display_name.clone(),
        resolved_path: resolved.to_string_lossy().to_string(),
        status,
        stored_fingerprint: source.fingerprint,
        disk_fingerprint: disk,
        moved_candidate,
        reapply_views,
        warnings,
    }
}

/// Build the open preview for a project file. Pure read: no state changes,
/// the file on disk is never modified.
pub(crate) fn open_preview(path: &Path) -> AppResult<ProjectOpenPreview> {
    let file = load_project_file(path)?;
    if file.template {
        return Err(AppError::invalid(
            "this file is a project TEMPLATE; use it to create a new project instead of opening it",
        ));
    }
    let dir = project_dir(path)?;
    let sources = file
        .sections
        .sources
        .iter()
        .map(|s| check_source(&dir, s))
        .collect();
    Ok(ProjectOpenPreview {
        path: path.to_string_lossy().to_string(),
        format_version: file.format_version.clone(),
        app_version: file.app_version.clone(),
        sources,
        tab_order: file.sections.tabs.open.clone(),
        active_tab: file.sections.tabs.active.clone(),
    })
}

/// Drop one source and everything referencing it.
fn remove_source(sections: &mut ProjectSections, id: &str) {
    sections.sources.retain(|s| s.id != id);
    sections.tabs.open.retain(|t| t != id);
    if sections.tabs.active.as_deref() == Some(id) {
        sections.tabs.active = None;
    }
    sections.views.retain(|v| v.source_id != id);
    sections.schemas.retain(|s| s.source_id != id);
    sections.row_keys.retain(|k| k.source_id != id);
    sections
        .join_mappings
        .retain(|j| j.left_source_id != id && j.right_source_id != id);
    sections
        .comparisons
        .retain(|c| c.left_source_id != id && c.right_source_id != id);
}

/// Resolve and apply a project open. All resolutions are validated BEFORE
/// any state is built, so a bad resolution cancels the whole open with the
/// project file untouched. Returns the in-memory project plus the plan the
/// front end executes through the ordinary open pipeline.
pub(crate) fn open_apply(
    path: &Path,
    resolutions: &[ResolutionEntry],
) -> AppResult<(OpenProject, ProjectOpenPlan)> {
    let file = load_project_file(path)?;
    if file.template {
        return Err(AppError::invalid(
            "this file is a project TEMPLATE; use it to create a new project instead of opening it",
        ));
    }
    let dir = project_dir(path)?;

    let mut by_id: HashMap<&str, &SourceResolution> = HashMap::new();
    for entry in resolutions {
        if !file
            .sections
            .sources
            .iter()
            .any(|s| s.id == entry.source_id)
        {
            return Err(AppError::invalid(format!(
                "resolution references unknown source \"{}\"",
                entry.source_id
            )));
        }
        by_id.insert(entry.source_id.as_str(), &entry.action);
    }

    let mut sections = file.sections.clone();
    let mut entries: HashMap<String, PlanEntry> = HashMap::new();
    let mut removed = Vec::new();
    let mut skipped = Vec::new();
    let mut dirty = false;

    for source in &file.sections.sources {
        let check = check_source(&dir, source);
        let action = by_id.get(source.id.as_str()).copied();
        match action {
            Some(SourceResolution::Remove) => {
                remove_source(&mut sections, &source.id);
                removed.push(source.id.clone());
                dirty = true;
            }
            Some(SourceResolution::Skip) => {
                skipped.push(source.id.clone());
            }
            Some(SourceResolution::Locate { path: replacement }) => {
                let located = decode_path(&dir, replacement);
                let Some(disk) = util::stat_fingerprint(&located) else {
                    return Err(AppError::invalid(format!(
                        "replacement file {} does not exist",
                        located.display()
                    )));
                };
                // Gate views against the located file: identical fingerprint
                // means the file simply moved; anything else re-runs the
                // column check on the new path.
                let (status, reapply_views, mut warnings) = if source.fingerprint == Some(disk) {
                    (SourceStatus::Ok, true, Vec::new())
                } else {
                    match column_check(&located, source) {
                        ColumnCheck::Incompatible(msg) => (
                            SourceStatus::SchemaIncompatible,
                            false,
                            vec![format!(
                                "the replacement file's columns do not match ({msg}); \
                                     saved views and schemas were not reapplied"
                            )],
                        ),
                        checked => {
                            let mut notes = Vec::new();
                            if let ColumnCheck::Unverifiable(reason) = &checked {
                                notes.push(format!(
                                    "could not verify column compatibility: {reason}"
                                ));
                            }
                            notes.push(
                                "the replacement file differs from what the project \
                                     saved; saved views were not reapplied automatically"
                                    .into(),
                            );
                            (SourceStatus::ChangedFingerprint, false, notes)
                        }
                    }
                };
                if let Some(s) = sections.sources.iter_mut().find(|s| s.id == source.id) {
                    s.path = located.to_string_lossy().to_string();
                    s.fingerprint = Some(disk);
                }
                dirty = true;
                warnings.splice(0..0, check.warnings.clone());
                entries.insert(
                    source.id.clone(),
                    PlanEntry {
                        source_id: source.id.clone(),
                        path: located.to_string_lossy().to_string(),
                        display_name: source.display_name.clone(),
                        open: source.open.clone(),
                        status,
                        reapply_views,
                        view_warnings: warnings,
                        views: Vec::new(),
                        active_view_id: None,
                    },
                );
            }
            Some(SourceResolution::Open) | None => {
                if matches!(
                    check.status,
                    SourceStatus::Missing | SourceStatus::MovedCandidate
                ) {
                    return Err(AppError::invalid(format!(
                        "source \"{}\" is missing from {}; locate a replacement, skip it, \
                         remove it, or cancel the open",
                        source.display_name.as_deref().unwrap_or(&source.id),
                        check.resolved_path
                    )));
                }
                entries.insert(
                    source.id.clone(),
                    PlanEntry {
                        source_id: source.id.clone(),
                        path: check.resolved_path.clone(),
                        display_name: source.display_name.clone(),
                        open: source.open.clone(),
                        status: check.status,
                        reapply_views: check.reapply_views,
                        view_warnings: check.warnings.clone(),
                        views: Vec::new(),
                        active_view_id: None,
                    },
                );
            }
        }
    }

    // Attach each source's saved named views (for reapplication after open)
    // from the post-removal sections. Schemas and row keys stay in their
    // persisted sections and are NOT surfaced in the open plan.
    for entry in entries.values_mut() {
        if let Some(v) = sections
            .views
            .iter()
            .find(|v| v.source_id == entry.source_id)
        {
            entry.views = v.views.clone();
            entry.active_view_id = v.active_view_id.clone();
        }
    }

    // Restore tab order over the sources actually being opened; sources not
    // listed in the saved tab order are appended in project order.
    let mut ordered: Vec<PlanEntry> = Vec::with_capacity(entries.len());
    for id in &file.sections.tabs.open {
        if let Some(entry) = entries.remove(id) {
            ordered.push(entry);
        }
    }
    for source in &file.sections.sources {
        if let Some(entry) = entries.remove(&source.id) {
            ordered.push(entry);
        }
    }
    let tab_order: Vec<String> = ordered.iter().map(|e| e.source_id.clone()).collect();
    let active_tab = file
        .sections
        .tabs
        .active
        .clone()
        .filter(|a| tab_order.contains(a))
        .or_else(|| tab_order.first().cloned());

    let mut project_file = file;
    project_file.sections = sections;
    let project = OpenProject {
        path: Some(path.to_path_buf()),
        file: project_file,
        revision: u64::from(dirty),
        saved_revision: 0,
    };
    let plan = ProjectOpenPlan {
        meta: project.meta(),
        entries: ordered,
        tab_order,
        active_tab,
        removed_source_ids: removed,
        skipped_source_ids: skipped,
    };
    Ok((project, plan))
}

// ---------------------------------------------------------------------------
// The store (dirty tracking, sections access, templates)
// ---------------------------------------------------------------------------

/// The one open project (or none), managed by Tauri.
#[derive(Default)]
pub struct ProjectStore(pub Mutex<Option<OpenProject>>);

/// The in-memory project: canonical content + revision-based dirty state.
/// Every section mutation bumps `revision`; the project is dirty while
/// `revision != saved_revision`.
#[derive(Debug, Clone)]
pub struct OpenProject {
    pub path: Option<PathBuf>,
    pub file: ProjectFile,
    pub revision: u64,
    pub saved_revision: u64,
}

impl OpenProject {
    fn new_empty() -> OpenProject {
        OpenProject {
            path: None,
            file: ProjectFile::new_empty(),
            revision: 0,
            saved_revision: 0,
        }
    }

    pub fn dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    pub fn meta(&self) -> ProjectMeta {
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Untitled project".to_string());
        ProjectMeta {
            path: self.path.as_ref().map(|p| p.to_string_lossy().to_string()),
            name,
            dirty: self.dirty(),
            revision: self.revision,
            format_version: self.file.format_version.clone(),
            app_version: self.file.app_version.clone(),
        }
    }
}

/// Project header state for the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectMeta {
    pub path: Option<String>,
    pub name: String,
    pub dirty: bool,
    pub revision: u64,
    pub format_version: String,
    pub app_version: String,
}

/// Full project state for the UI: meta + every section as JSON.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectStateDto {
    pub meta: ProjectMeta,
    pub sections: Value,
}

/// Typed write of one registered section. Reserved sections and unknown
/// names are rejected; the payload must both deserialize into the section's
/// registered type AND pass the no-cell-data scan.
fn set_section_typed(sections: &mut ProjectSections, name: &str, value: Value) -> AppResult<()> {
    if let Some(spec) = SECTION_REGISTRY.iter().find(|s| s.name == name) {
        if spec.reserved {
            return Err(AppError::invalid(format!(
                "section \"{name}\" is reserved for a future feature ({}) and cannot be \
                 written yet",
                spec.owner
            )));
        }
    } else {
        let known: Vec<&str> = SECTION_REGISTRY.iter().map(|s| s.name).collect();
        return Err(AppError::invalid(format!(
            "unknown project section \"{name}\" (known sections: {})",
            known.join(", ")
        )));
    }
    scan_for_data_keys(name, &value)?;
    let shape = |e: serde_json::Error| {
        AppError::invalid(format!(
            "invalid payload for project section \"{name}\": {e}"
        ))
    };
    match name {
        "sources" => sections.sources = serde_json::from_value(value).map_err(shape)?,
        "tabs" => sections.tabs = serde_json::from_value(value).map_err(shape)?,
        "layout" => sections.layout = if value.is_null() { None } else { Some(value) },
        "views" => sections.views = serde_json::from_value(value).map_err(shape)?,
        "profiles" => sections.profiles = serde_json::from_value(value).map_err(shape)?,
        "schemas" => sections.schemas = serde_json::from_value(value).map_err(shape)?,
        "recipes" => sections.recipes = serde_json::from_value(value).map_err(shape)?,
        "joinMappings" => sections.join_mappings = serde_json::from_value(value).map_err(shape)?,
        "comparisons" => sections.comparisons = serde_json::from_value(value).map_err(shape)?,
        "rowKeys" => sections.row_keys = serde_json::from_value(value).map_err(shape)?,
        _ => unreachable!("registry check above covers every arm"),
    }
    Ok(())
}

/// Whether a string looks like an absolute filesystem path under EITHER
/// convention (so a Windows-authored pattern is still detected on a Linux
/// build and vice versa): a POSIX root, a UNC prefix, or a `C:\`/`C:/` drive
/// prefix. Used to catch a `Glob` pattern that embeds a concrete location.
fn looks_absolute(s: &str) -> bool {
    if s.starts_with('/') || s.starts_with('\\') {
        return true; // POSIX root or UNC/backslash root
    }
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'/' || b[2] == b'\\')
}

/// Rewrite a profile matcher into a portable, path-free form. `ExactPath`
/// and `Directory` embed a literal absolute path, and a `Glob` pattern can
/// too; each is generalized (an `ExactPath` keeps its file extension when it
/// has one, verified separator-free) so a template never carries the authoring
/// machine's layout. Any other matcher (`Extension`, a relative `Glob`) is
/// left untouched.
fn portable_matcher(matcher: &ProfileMatch) -> ProfileMatch {
    let wildcard = || ProfileMatch::Glob {
        pattern: "*".to_string(),
    };
    // A path's final `.segment`, but only when it is a real extension
    // (non-empty and carrying no path separator), computed without relying on
    // the host OS's separator so a Windows path scrubs the same way on Linux.
    let extension_of = |path: &str| -> Option<String> {
        let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
        match name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() => Some(ext.to_string()),
            _ => None,
        }
    };
    match matcher {
        ProfileMatch::ExactPath { path } => match extension_of(path) {
            Some(ext) => ProfileMatch::Extension { extension: ext },
            None => wildcard(),
        },
        ProfileMatch::Directory { .. } => wildcard(),
        ProfileMatch::Glob { pattern } if looks_absolute(pattern) => wildcard(),
        other => other.clone(),
    }
}

/// Strip everything that references concrete source files, leaving pure
/// configuration: this is what "save as template" writes and what
/// new-from-template starts from. Bundled profiles are kept (their validation
/// rules are portable) but their matchers are scrubbed of any absolute path,
/// so a template never leaks a filesystem layout regardless of section.
/// Unknown sections are preserved — the no-cell-data scan already guarantees
/// they carry configuration only.
fn strip_sources(sections: &mut ProjectSections) {
    sections.sources.clear();
    sections.tabs = TabsSection::default();
    sections.views.clear();
    sections.schemas.clear();
    sections.row_keys.clear();
    sections.join_mappings.clear();
    sections.comparisons.clear();
    for profile in &mut sections.profiles {
        profile.matcher = portable_matcher(&profile.matcher);
    }
}

/// Create a new project, optionally initialized from a template (or any
/// project file — its sources are stripped either way).
pub(crate) fn new_project(template_path: Option<&Path>) -> AppResult<OpenProject> {
    let mut project = OpenProject::new_empty();
    if let Some(path) = template_path {
        let file = load_project_file(path)?;
        project.file = file;
        project.file.template = false;
        project.file.format_version = current_format_version();
        project.file.app_version = app_version();
        strip_sources(&mut project.file.sections);
    }
    Ok(project)
}

/// Write the open project to its path (must have one).
pub(crate) fn save_project(project: &mut OpenProject) -> AppResult<()> {
    let Some(path) = project.path.clone() else {
        return Err(AppError::invalid(
            "the project has no file yet; use save-as to choose a location",
        ));
    };
    write_project_file(&path, &project.file)?;
    project.saved_revision = project.revision;
    Ok(())
}

/// Write a template copy (sources stripped, `template: true`) of the open
/// project to `path`. The open project itself is not modified.
pub(crate) fn save_template(project: &OpenProject, path: &Path) -> AppResult<()> {
    let mut copy = project.file.clone();
    copy.template = true;
    copy.app_version = app_version();
    strip_sources(&mut copy.sections);
    write_project_file(path, &copy)
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

type Store<'a> = State<'a, ProjectStore>;

fn lock_store<'g>(store: &'g Store<'_>) -> AppResult<MutexGuard<'g, Option<OpenProject>>> {
    store
        .0
        .lock()
        .map_err(|_| AppError::Other("internal project lock error".into()))
}

fn require_open<'g>(
    guard: &'g mut MutexGuard<'_, Option<OpenProject>>,
) -> AppResult<&'g mut OpenProject> {
    guard
        .as_mut()
        .ok_or_else(|| AppError::invalid("no project is open"))
}

/// Create a new (unsaved) project, optionally from a template file,
/// replacing any open project. The front end confirms discarding unsaved
/// changes BEFORE calling this.
#[tauri::command]
pub fn project_new(
    template_path: Option<String>,
    store: Store<'_>,
) -> Result<ProjectMeta, AppError> {
    let project = new_project(template_path.as_deref().map(Path::new))?;
    let meta = project.meta();
    *lock_store(&store)? = Some(project);
    Ok(meta)
}

/// The current project (meta + all sections), or `None`.
#[tauri::command]
pub fn project_get(store: Store<'_>) -> Result<Option<ProjectStateDto>, AppError> {
    let guard = lock_store(&store)?;
    let Some(project) = guard.as_ref() else {
        return Ok(None);
    };
    let sections = serde_json::to_value(&project.file.sections)
        .map_err(|e| AppError::Other(format!("project serialization failed: {e}")))?;
    Ok(Some(ProjectStateDto {
        meta: project.meta(),
        sections,
    }))
}

/// Replace one registered section. Bumps the project revision (dirty) only
/// when the value actually changed.
#[tauri::command]
pub fn project_set_section(
    name: String,
    value: Value,
    store: Store<'_>,
) -> Result<ProjectMeta, AppError> {
    let mut guard = lock_store(&store)?;
    let project = require_open(&mut guard)?;
    let before = serde_json::to_value(&project.file.sections)
        .map_err(|e| AppError::Other(format!("project serialization failed: {e}")))?;
    set_section_typed(&mut project.file.sections, &name, value)?;
    let after = serde_json::to_value(&project.file.sections)
        .map_err(|e| AppError::Other(format!("project serialization failed: {e}")))?;
    if before != after {
        project.revision += 1;
    }
    Ok(project.meta())
}

/// Save the project to its existing path (atomic).
#[tauri::command]
pub fn project_save(store: Store<'_>) -> Result<ProjectMeta, AppError> {
    let mut guard = lock_store(&store)?;
    let project = require_open(&mut guard)?;
    save_project(project)?;
    Ok(project.meta())
}

/// Save the project to a new path (atomic) and adopt it. Source paths are
/// re-relativized against the new location on write.
#[tauri::command]
pub fn project_save_as(path: String, store: Store<'_>) -> Result<ProjectMeta, AppError> {
    let mut guard = lock_store(&store)?;
    let project = require_open(&mut guard)?;
    let previous = project.path.replace(PathBuf::from(path));
    if let Err(e) = save_project(project) {
        // The failed save wrote nothing; do not adopt the new path either.
        project.path = previous;
        return Err(e);
    }
    Ok(project.meta())
}

/// Export the open project as a reusable template (configuration only, no
/// source paths). The open project itself is untouched.
#[tauri::command]
pub fn project_save_template(path: String, store: Store<'_>) -> Result<(), AppError> {
    let mut guard = lock_store(&store)?;
    let project = require_open(&mut guard)?;
    save_template(project, Path::new(&path))
}

/// Close the project (documents stay open; they simply stop belonging to a
/// project). The front end confirms discarding unsaved changes first.
#[tauri::command]
pub fn project_close(store: Store<'_>) -> Result<(), AppError> {
    *lock_store(&store)? = None;
    Ok(())
}

/// Preview opening a project file: per-source statuses and gating verdicts.
/// Pure read — no state changes until `project_open_apply`.
#[tauri::command]
pub fn project_open_preview(path: String) -> Result<ProjectOpenPreview, AppError> {
    open_preview(Path::new(&path))
}

/// Apply a project open with the user's per-source resolutions, replacing
/// any open project. Cancelling is simply never calling this.
#[tauri::command]
pub fn project_open_apply(
    path: String,
    resolutions: Vec<ResolutionEntry>,
    store: Store<'_>,
) -> Result<ProjectOpenPlan, AppError> {
    let (project, plan) = open_apply(Path::new(&path), &resolutions)?;
    *lock_store(&store)? = Some(project);
    Ok(plan)
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compare::CompareMode;
    use crate::row_identity::KeyNormalization;
    use crate::schema::{ColumnSchema, LogicalType, SCHEMA_VERSION};
    use crate::settings::ProfileMatch;

    fn write_csv(dir: &Path, name: &str, content: &str) -> PathBuf {
        // Build the path from native components so the returned `PathBuf` uses
        // the platform separator throughout (a literal "data/a.csv" argument to
        // `join` would keep its `/` on Windows, giving a mixed-separator string
        // that no longer equals the separator-normalized path a disk round-trip
        // produces). This keeps the round-trip an exact identity on every OS.
        let mut path = dir.to_path_buf();
        for part in name.split('/') {
            path.push(part);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&path, content).unwrap();
        path
    }

    fn source_for(id: &str, path: &Path) -> ProjectSource {
        ProjectSource {
            id: id.to_string(),
            path: path.to_string_lossy().to_string(),
            display_name: Some(format!("{id}.csv")),
            fingerprint: util::stat_fingerprint(path),
            open: ProjectOpenSettings {
                delimiter: Some(",".into()),
                encoding: Some("UTF-8".into()),
                has_header_row: Some(true),
                ..Default::default()
            },
            columns: vec![
                ProjectColumn {
                    id: "c0".into(),
                    name: "id".into(),
                },
                ProjectColumn {
                    id: "c1".into(),
                    name: "amount".into(),
                },
            ],
            unknown: BTreeMap::new(),
        }
    }

    fn a_view(id: &str) -> NamedView {
        NamedView {
            id: id.into(),
            name: format!("view {id}"),
            filter: None,
            filter_column_ids: vec!["c0".into(), "c1".into()],
            sort_keys: Vec::new(),
            hidden_column_ids: vec!["c1".into()],
            pinned_column_ids: Vec::new(),
            column_order: Vec::new(),
            column_widths: std::collections::HashMap::new(),
            wrap_text: false,
        }
    }

    fn a_profile() -> FileProfile {
        FileProfile {
            id: "p1".into(),
            name: "orders".into(),
            matcher: ProfileMatch::Extension {
                extension: "csv".into(),
            },
            auto_apply: false,
            delimiter: Some(",".into()),
            encoding: None,
            has_header_row: Some(true),
            default_export: None,
            expected_columns: vec!["id".into(), "amount".into()],
            enforce_order: false,
            expected_types: Vec::new(),
            required_columns: Vec::new(),
            unique_columns: Vec::new(),
            regex_rules: Vec::new(),
            range_rules: Vec::new(),
            semantic_types: Vec::new(),
            cross_rules: Vec::new(),
            named_views: Vec::new(),
            last_view_id: None,
        }
    }

    /// A fully populated project rooted at `dir` with two on-disk sources.
    fn full_project(dir: &Path) -> (ProjectFile, PathBuf, PathBuf) {
        let a = write_csv(dir, "data/a.csv", "id,amount\n1,10\n");
        let b = write_csv(dir, "b.csv", "id,amount\n2,20\n");
        let mut file = ProjectFile::new_empty();
        let s = &mut file.sections;
        s.sources = vec![source_for("srcA", &a), source_for("srcB", &b)];
        s.tabs = TabsSection {
            open: vec!["srcB".into(), "srcA".into()],
            active: Some("srcA".into()),
            ..Default::default()
        };
        s.layout = Some(serde_json::json!({"sidebar": "left", "width": 320}));
        s.views = vec![SourceViews {
            source_id: "srcA".into(),
            views: vec![a_view("v1")],
            active_view_id: Some("v1".into()),
        }];
        s.profiles = vec![a_profile()];
        s.schemas = vec![SourceSchema {
            source_id: "srcA".into(),
            schema: SchemaExport {
                version: SCHEMA_VERSION,
                columns: vec![ColumnSchema::new("c1", "amount", LogicalType::Integer)],
            },
        }];
        s.recipes = vec![NamedConfig {
            id: "r1".into(),
            name: "clean".into(),
            config: serde_json::json!({"version": 1, "name": "clean", "steps": []}),
        }];
        s.join_mappings = vec![JoinMapping {
            id: "j1".into(),
            name: "a+b".into(),
            left_source_id: "srcA".into(),
            right_source_id: "srcB".into(),
            spec: serde_json::json!({"join": "left", "leftKeys": [0], "rightKeys": [0],
                                     "rightColumns": [1]}),
        }];
        s.comparisons = vec![ComparisonDef {
            id: "cmp1".into(),
            name: "a vs b".into(),
            left_source_id: "srcA".into(),
            right_source_id: "srcB".into(),
            spec: CompareSpec {
                mode: CompareMode::Keyed,
                key_columns: vec![0],
                column_mapping: Vec::new(),
                trim: true,
                case_insensitive: false,
                blank_equal: false,
                numeric_equal: false,
                date_equal: false,
            },
        }];
        s.row_keys = vec![SourceRowKey {
            source_id: "srcA".into(),
            key: KeySpec {
                columns: vec!["c0".into()],
                normalization: KeyNormalization {
                    trim: true,
                    case_fold: true,
                    unicode_nfkc: false,
                },
            },
        }];
        (file, a, b)
    }

    // ----- versioning ---------------------------------------------------------

    #[test]
    fn format_version_parses_and_rejects_garbage() {
        assert_eq!(format_major("1.0").unwrap(), 1);
        assert_eq!(format_major("2.13").unwrap(), 2);
        assert!(format_major("banana").is_err());
        assert!(format_major("").is_err());
    }

    #[test]
    fn newer_major_is_rejected_with_a_clear_error_and_newer_minor_is_accepted() {
        let two = br#"{"formatVersion":"2.0","appVersion":"9.9.9","sections":{}}"#;
        let err = parse_project_bytes(two).unwrap_err().to_string();
        assert!(err.contains("newer version"), "unexpected error: {err}");
        assert!(err.contains("2.0"), "names the offending version: {err}");

        let minor = br#"{"formatVersion":"1.9","appVersion":"9.9.9","sections":{}}"#;
        let file = parse_project_bytes(minor).unwrap();
        assert_eq!(file.format_version, "1.9", "newer minor loads and is kept");
    }

    #[test]
    fn corrupt_project_errors_clearly_and_leaves_the_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.ceesveeproj");
        std::fs::write(&path, b"{ not json !!!").unwrap();

        let err = open_preview(&path).unwrap_err().to_string();
        assert!(
            err.contains("not a valid CEESVEE project file"),
            "unexpected error: {err}"
        );
        // Unlike settings, the user's project file is NEVER moved or altered.
        assert_eq!(std::fs::read(&path).unwrap(), b"{ not json !!!");
    }

    // ----- round trip ---------------------------------------------------------

    #[test]
    fn round_trip_preserves_every_section_and_relativizes_paths() {
        let dir = tempfile::tempdir().unwrap();
        let (file, _a, _b) = full_project(dir.path());
        let path = dir.path().join(format!("work.{PROJECT_EXTENSION}"));
        write_project_file(&path, &file).unwrap();

        // On disk: both sources sit under the project dir, so both paths are
        // stored relative, with forward slashes.
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            raw.contains("\"data/a.csv\""),
            "relative with forward slashes: {raw}"
        );
        assert!(raw.contains("\"b.csv\""), "sibling stays relative: {raw}");

        // Reloaded: absolute again, and every section survives verbatim.
        let loaded = load_project_file(&path).unwrap();
        assert_eq!(loaded.sections, file.sections);
        assert!(Path::new(&loaded.sections.sources[0].path).is_absolute());
    }

    #[test]
    fn unknown_fields_and_sections_survive_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("future.ceesveeproj");
        std::fs::write(
            &path,
            br#"{
              "formatVersion": "1.7",
              "appVersion": "9.1.0",
              "futureTopLevel": {"keep": true},
              "sections": {
                "annotations": [{"rowKey": ["1"], "note": "check this"}],
                "futureSection": {"nested": [1, 2, 3]},
                "sources": [{"id": "s1", "path": "x.csv", "futureSourceField": "keep-me"}]
              }
            }"#,
        )
        .unwrap();
        write_csv(dir.path(), "x.csv", "a,b\n1,2\n");

        let loaded = load_project_file(&path).unwrap();
        let out = dir.path().join("resaved.ceesveeproj");
        write_project_file(&out, &loaded).unwrap();
        let raw = std::fs::read_to_string(&out).unwrap();
        assert!(
            raw.contains("\"futureTopLevel\""),
            "envelope extras kept: {raw}"
        );
        assert!(
            raw.contains("\"futureSection\""),
            "unknown sections kept: {raw}"
        );
        assert!(
            raw.contains("\"futureSourceField\""),
            "source extras kept: {raw}"
        );
        assert!(
            raw.contains("\"1.7\""),
            "newer-minor version string kept: {raw}"
        );
        assert!(
            raw.contains("\"note\": \"check this\""),
            "reserved content kept: {raw}"
        );
    }

    #[test]
    fn nested_unknown_fields_in_tabs_and_open_survive_a_round_trip() {
        // Unknown fields a future minor adds INSIDE the tabs section and inside
        // a source's open-settings object must survive re-save (the flatten
        // catch-alls on those F37-owned types).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested.ceesveeproj");
        std::fs::write(
            &path,
            br#"{
              "formatVersion": "1.4",
              "appVersion": "9.1.0",
              "sections": {
                "tabs": {"open": ["s1"], "active": "s1", "futureTabsField": {"k": 1}},
                "sources": [{
                  "id": "s1",
                  "path": "x.csv",
                  "open": {"delimiter": ",", "futureOpenField": "keep-me"}
                }]
              }
            }"#,
        )
        .unwrap();
        write_csv(dir.path(), "x.csv", "a,b\n1,2\n");

        let loaded = load_project_file(&path).unwrap();
        let out = dir.path().join("resaved.ceesveeproj");
        write_project_file(&out, &loaded).unwrap();
        let raw = std::fs::read_to_string(&out).unwrap();
        assert!(
            raw.contains("\"futureTabsField\""),
            "tabs-level extras kept: {raw}"
        );
        assert!(
            raw.contains("\"futureOpenField\""),
            "open-settings extras kept: {raw}"
        );
    }

    // ----- paths --------------------------------------------------------------

    #[test]
    fn make_relative_walks_up_and_falls_back_across_roots() {
        #[cfg(windows)]
        {
            let rel = make_relative(Path::new(r"C:\proj\ws"), Path::new(r"C:\data\in.csv"));
            assert_eq!(rel.unwrap(), Path::new(r"..\..\data\in.csv"));
            // Drive letters compare case-insensitively.
            let rel = make_relative(Path::new(r"c:\proj"), Path::new(r"C:\proj\a.csv"));
            assert_eq!(rel.unwrap(), Path::new("a.csv"));
            // Cross-drive: no relative form exists.
            assert!(make_relative(Path::new(r"C:\proj"), Path::new(r"D:\data\in.csv")).is_none());
            let encoded = encode_path(Path::new(r"C:\proj"), Path::new(r"D:\data\in.csv"));
            assert_eq!(encoded, r"D:\data\in.csv", "absolute fallback");
        }
        #[cfg(not(windows))]
        {
            let rel = make_relative(Path::new("/proj/ws"), Path::new("/data/in.csv"));
            assert_eq!(rel.unwrap(), Path::new("../../data/in.csv"));
            let rel = make_relative(Path::new("/proj"), Path::new("/proj/sub/a.csv"));
            assert_eq!(rel.unwrap(), Path::new("sub/a.csv"));
        }
        // Relative inputs never relativize (we only store decoded absolutes).
        assert!(make_relative(Path::new("rel"), Path::new("also/rel")).is_none());
    }

    #[test]
    fn encode_decode_round_trips_relative_and_absolute() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let inside = base.join("sub").join("f.csv");
        let encoded = encode_path(base, &inside);
        assert_eq!(encoded, "sub/f.csv", "forward slashes on every platform");
        assert_eq!(decode_path(base, &encoded), inside);

        // An absolute stored path passes through decode unchanged.
        let abs = inside.to_string_lossy().to_string();
        assert_eq!(decode_path(base, &abs), inside);
    }

    // ----- no cell data -------------------------------------------------------

    #[test]
    fn a_normal_project_file_contains_no_cell_data_keys() {
        let dir = tempfile::tempdir().unwrap();
        let (file, _, _) = full_project(dir.path());
        let path = dir.path().join("scan.ceesveeproj");
        write_project_file(&path, &file).unwrap();

        // The acceptance scan: walk the ACTUAL serialized JSON for any
        // data-bearing key.
        let value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        scan_for_data_keys("whole file", &value).expect("no forbidden keys anywhere");
    }

    #[test]
    fn data_bearing_sections_are_rejected_at_set_and_at_save() {
        let mut sections = ProjectSections::default();
        let err = set_section_typed(
            &mut sections,
            "layout",
            serde_json::json!({"panel": {"cells": [["a", "b"]]}}),
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("cells"), "names the offending key: {err}");

        // Defense in depth: a data-bearing UNKNOWN section (e.g. loaded from
        // a tampered file) is caught by the save-time scan.
        let dir = tempfile::tempdir().unwrap();
        let mut file = ProjectFile::new_empty();
        file.sections
            .unknown
            .insert("smuggled".into(), serde_json::json!({"rows": [["1", "2"]]}));
        let path = dir.path().join("nope.ceesveeproj");
        let err = write_project_file(&path, &file).unwrap_err().to_string();
        assert!(err.contains("rows"), "save refuses: {err}");
        assert!(!path.exists(), "nothing was written");
    }

    #[test]
    fn reserved_and_unknown_section_writes_are_rejected() {
        let mut sections = ProjectSections::default();
        for reserved in ["annotations", "dictionary", "queries"] {
            let err = set_section_typed(&mut sections, reserved, serde_json::json!([]))
                .unwrap_err()
                .to_string();
            assert!(err.contains("reserved"), "{reserved}: {err}");
        }
        let err = set_section_typed(&mut sections, "nope", serde_json::json!([]))
            .unwrap_err()
            .to_string();
        assert!(err.contains("known sections"), "lists the registry: {err}");

        // A typed section validates its shape.
        let err = set_section_typed(&mut sections, "sources", serde_json::json!("not a list"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("sources"), "names the section: {err}");
    }

    // ----- dirty tracking -----------------------------------------------------

    #[test]
    fn section_mutations_drive_dirty_state_and_save_clears_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut project = new_project(None).unwrap();
        assert!(!project.dirty());

        let tabs = serde_json::json!({"open": ["s1"], "active": "s1"});
        set_section_typed(&mut project.file.sections, "tabs", tabs).unwrap();
        project.revision += 1; // what project_set_section does on change
        assert!(project.dirty());

        project.path = Some(dir.path().join("p.ceesveeproj"));
        save_project(&mut project).unwrap();
        assert!(!project.dirty(), "saving clears dirty");
        assert!(project.path.as_ref().unwrap().exists());
    }

    #[test]
    fn setting_an_identical_section_does_not_mark_the_project_dirty() {
        // Mirrors the equality check in project_set_section.
        let mut sections = ProjectSections::default();
        let tabs = serde_json::json!({"open": [], "active": null});
        let before = serde_json::to_value(&sections).unwrap();
        set_section_typed(&mut sections, "tabs", tabs).unwrap();
        let after = serde_json::to_value(&sections).unwrap();
        assert_eq!(before, after, "no-op writes are detectable as unchanged");
    }

    // ----- open flow ----------------------------------------------------------

    #[test]
    fn preview_reports_ok_missing_changed_and_schema_incompatible() {
        let dir = tempfile::tempdir().unwrap();
        let (mut file, _a, b) = full_project(dir.path());
        // Add two more sources: one that will go missing, one that will change.
        let c = write_csv(dir.path(), "c.csv", "id,amount\n3,30\n");
        let d = write_csv(dir.path(), "d.csv", "id,amount\n4,40\n");
        file.sections.sources.push(source_for("srcC", &c));
        file.sections.sources.push(source_for("srcD", &d));
        let path = dir.path().join("p.ceesveeproj");
        write_project_file(&path, &file).unwrap();

        std::fs::remove_file(&c).unwrap();
        // Same columns, different content AND size -> changed, not incompatible.
        std::fs::write(&d, "id,amount\n4,40\n5,50\n").unwrap();
        // Renamed column -> schema-incompatible.
        std::fs::write(&b, "id,total\n2,20\n").unwrap();

        let preview = open_preview(&path).unwrap();
        let by_id = |id: &str| preview.sources.iter().find(|s| s.source_id == id).unwrap();

        let a = by_id("srcA");
        assert_eq!(a.status, SourceStatus::Ok);
        assert!(a.reapply_views, "untouched source reapplies views");
        assert!(a.warnings.is_empty());

        let c = by_id("srcC");
        assert_eq!(c.status, SourceStatus::Missing);
        assert!(!c.reapply_views);

        let d = by_id("srcD");
        assert_eq!(d.status, SourceStatus::ChangedFingerprint);
        assert!(!d.reapply_views, "changed file gates views");
        assert!(!d.warnings.is_empty(), "warns instead of breaking");

        let b = by_id("srcB");
        assert_eq!(b.status, SourceStatus::SchemaIncompatible);
        assert!(!b.reapply_views);
        assert!(
            b.warnings.iter().any(|w| w.contains("total")),
            "explains the mismatch: {:?}",
            b.warnings
        );

        // Tab order and active tab come through for the dialog.
        assert_eq!(preview.tab_order, vec!["srcB", "srcA"]);
        assert_eq!(preview.active_tab.as_deref(), Some("srcA"));
    }

    #[test]
    fn preview_offers_a_moved_candidate_and_locate_relinks_it() {
        let dir = tempfile::tempdir().unwrap();
        let (file, a, _b) = full_project(dir.path());
        let path = dir.path().join("p.ceesveeproj");
        write_project_file(&path, &file).unwrap();

        // Move a.csv from data/ into a fresh subdirectory next to the project.
        let moved_to = dir.path().join("moved");
        std::fs::create_dir_all(&moved_to).unwrap();
        let new_home = moved_to.join("a.csv");
        std::fs::rename(&a, &new_home).unwrap();

        let preview = open_preview(&path).unwrap();
        let entry = preview
            .sources
            .iter()
            .find(|s| s.source_id == "srcA")
            .unwrap();
        assert_eq!(entry.status, SourceStatus::MovedCandidate);
        let candidate = entry.moved_candidate.clone().expect("candidate offered");
        assert_eq!(PathBuf::from(&candidate), new_home);

        // Locate -> relinked, opened, project dirty, stored path updated.
        let resolutions = vec![ResolutionEntry {
            source_id: "srcA".into(),
            action: SourceResolution::Locate { path: candidate },
        }];
        let (project, plan) = open_apply(&path, &resolutions).unwrap();
        assert!(project.dirty(), "relinking is a metadata change");
        let relinked = project
            .file
            .sections
            .sources
            .iter()
            .find(|s| s.id == "srcA")
            .unwrap();
        assert_eq!(PathBuf::from(&relinked.path), new_home);
        let planned = plan.entries.iter().find(|e| e.source_id == "srcA").unwrap();
        assert_eq!(PathBuf::from(&planned.path), new_home);
        assert_eq!(planned.status, SourceStatus::Ok, "same bytes, just moved");
        assert!(planned.reapply_views, "a clean move keeps views");
    }

    #[test]
    fn apply_resolution_matrix_removes_skips_and_opens() {
        let dir = tempfile::tempdir().unwrap();
        let (mut file, _a, _b) = full_project(dir.path());
        let c = write_csv(dir.path(), "c.csv", "id,amount\n3,30\n");
        file.sections.sources.push(source_for("srcC", &c));
        file.sections.tabs.open.push("srcC".into());
        let path = dir.path().join("p.ceesveeproj");
        write_project_file(&path, &file).unwrap();
        std::fs::remove_file(&c).unwrap();

        let resolutions = vec![
            ResolutionEntry {
                source_id: "srcC".into(),
                action: SourceResolution::Remove,
            },
            ResolutionEntry {
                source_id: "srcB".into(),
                action: SourceResolution::Skip,
            },
        ];
        let (project, plan) = open_apply(&path, &resolutions).unwrap();

        assert_eq!(plan.removed_source_ids, vec!["srcC"]);
        assert_eq!(plan.skipped_source_ids, vec!["srcB"]);
        let opened: Vec<&str> = plan.entries.iter().map(|e| e.source_id.as_str()).collect();
        assert_eq!(opened, vec!["srcA"], "missing removed, skipped not opened");
        assert!(project.dirty(), "removal is a metadata change");

        // The removal cascaded through every referencing section...
        let s = &project.file.sections;
        assert!(s.sources.iter().all(|x| x.id != "srcC"));
        assert!(!s.tabs.open.contains(&"srcC".to_string()));
        // ...while srcB (merely skipped) keeps its config.
        assert!(s.sources.iter().any(|x| x.id == "srcB"));
        assert!(s.join_mappings.iter().any(|j| j.right_source_id == "srcB"));

        // The plan carries the per-source named views for reapplication...
        let a = &plan.entries[0];
        assert_eq!(a.views.len(), 1);
        assert_eq!(a.active_view_id.as_deref(), Some("v1"));
        assert_eq!(plan.active_tab.as_deref(), Some("srcA"));
        // ...while schemas and row keys stay in the persisted sections (they
        // are consumed on demand, never reapplied as a side effect of open).
        assert!(project
            .file
            .sections
            .schemas
            .iter()
            .any(|s| s.source_id == "srcA"));
        assert!(project
            .file
            .sections
            .row_keys
            .iter()
            .any(|k| k.source_id == "srcA"));
    }

    #[test]
    fn removing_a_source_cascades_through_join_mappings_and_comparisons() {
        let dir = tempfile::tempdir().unwrap();
        let (file, _, _) = full_project(dir.path());
        let mut sections = file.sections;
        remove_source(&mut sections, "srcB");
        assert!(sections.join_mappings.is_empty(), "join referenced srcB");
        assert!(
            sections.comparisons.is_empty(),
            "comparison referenced srcB"
        );
        assert!(sections.views.len() == 1, "srcA's views survive");
        assert!(sections.sources.iter().all(|s| s.id != "srcB"));
    }

    #[test]
    fn a_missing_source_without_a_resolution_cancels_the_whole_open() {
        let dir = tempfile::tempdir().unwrap();
        let (file, a, _b) = full_project(dir.path());
        let path = dir.path().join("p.ceesveeproj");
        write_project_file(&path, &file).unwrap();
        std::fs::remove_file(&a).unwrap();

        let err = open_apply(&path, &[]).unwrap_err().to_string();
        assert!(err.contains("missing"), "explains the block: {err}");
        assert!(err.contains("srcA.csv"), "names the source: {err}");
        // The project file itself is untouched by the failed open.
        assert!(load_project_file(&path).is_ok());
    }

    #[test]
    fn resolutions_for_unknown_sources_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let (file, _, _) = full_project(dir.path());
        let path = dir.path().join("p.ceesveeproj");
        write_project_file(&path, &file).unwrap();
        let err = open_apply(
            &path,
            &[ResolutionEntry {
                source_id: "ghost".into(),
                action: SourceResolution::Remove,
            }],
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("ghost"), "{err}");
    }

    #[test]
    fn sources_without_fingerprints_open_but_never_reapply_views() {
        let dir = tempfile::tempdir().unwrap();
        let (mut file, a, _) = full_project(dir.path());
        file.sections.sources[0].fingerprint = None;
        let path = dir.path().join("p.ceesveeproj");
        write_project_file(&path, &file).unwrap();
        let _keep = a;

        let preview = open_preview(&path).unwrap();
        let entry = preview
            .sources
            .iter()
            .find(|s| s.source_id == "srcA")
            .unwrap();
        assert_eq!(entry.status, SourceStatus::Ok, "the file itself is fine");
        assert!(!entry.reapply_views, "no fingerprint -> no automatic views");
        assert!(!entry.warnings.is_empty());
    }

    // ----- templates ----------------------------------------------------------

    #[test]
    fn templates_strip_sources_and_initialize_a_repeatable_workflow() {
        let dir = tempfile::tempdir().unwrap();
        let (file, _, _) = full_project(dir.path());
        let project = OpenProject {
            path: Some(dir.path().join("p.ceesveeproj")),
            file,
            revision: 0,
            saved_revision: 0,
        };
        let template_path = dir.path().join("workflow.ceesveeproj");
        save_template(&project, &template_path).unwrap();

        // The template file: marked, and free of anything source-bound.
        let raw: Value = serde_json::from_slice(&std::fs::read(&template_path).unwrap()).unwrap();
        assert_eq!(raw["template"], Value::Bool(true));
        let sections = &raw["sections"];
        assert_eq!(sections["sources"], serde_json::json!([]));
        assert_eq!(sections["views"], serde_json::json!([]));
        assert_eq!(sections["schemas"], serde_json::json!([]));
        assert_eq!(sections["rowKeys"], serde_json::json!([]));
        assert_eq!(sections["joinMappings"], serde_json::json!([]));
        assert_eq!(sections["comparisons"], serde_json::json!([]));
        assert_eq!(sections["tabs"]["open"], serde_json::json!([]));
        // Configuration survives: profiles, recipes, layout.
        assert_eq!(sections["profiles"][0]["name"], "orders");
        assert_eq!(sections["recipes"][0]["name"], "clean");
        assert_eq!(sections["layout"]["sidebar"], "left");

        // A template cannot be opened as a project...
        let err = open_preview(&template_path).unwrap_err().to_string();
        assert!(err.contains("TEMPLATE"), "{err}");

        // ...but it initializes a fresh, unsaved, non-template project.
        let fresh = new_project(Some(&template_path)).unwrap();
        assert!(fresh.path.is_none());
        assert!(!fresh.file.template);
        assert!(!fresh.dirty());
        assert_eq!(fresh.file.sections.profiles.len(), 1);
        assert_eq!(fresh.file.sections.recipes.len(), 1);
        assert!(fresh.file.sections.sources.is_empty());
        assert_eq!(fresh.meta().name, "Untitled project");
    }

    #[test]
    fn templates_scrub_absolute_paths_from_bundled_profiles() {
        // A profile whose matcher embeds an absolute path must never ride into
        // a portable template: ExactPath with an extension generalizes to that
        // extension, and Directory / an absolute Glob collapse to "*".
        let dir = tempfile::tempdir().unwrap();
        let (mut file, _, _) = full_project(dir.path());
        let secret_dir = r"C:\Users\alice\secret";
        let secret_file = r"C:\Users\alice\secret\orders.csv";
        let mut exact = a_profile();
        exact.id = "exact".into();
        exact.matcher = ProfileMatch::ExactPath {
            path: secret_file.into(),
        };
        let mut directory = a_profile();
        directory.id = "dir".into();
        directory.matcher = ProfileMatch::Directory {
            directory: secret_dir.into(),
        };
        let mut abs_glob = a_profile();
        abs_glob.id = "glob".into();
        abs_glob.matcher = ProfileMatch::Glob {
            pattern: r"C:\Users\alice\secret\*.csv".into(),
        };
        file.sections.profiles = vec![exact, directory, abs_glob];

        let project = OpenProject {
            path: Some(dir.path().join("p.ceesveeproj")),
            file,
            revision: 0,
            saved_revision: 0,
        };
        let template_path = dir.path().join("workflow.ceesveeproj");
        save_template(&project, &template_path).unwrap();

        let raw = std::fs::read_to_string(&template_path).unwrap();
        assert!(
            !raw.contains("alice"),
            "no fragment of the authoring path survives: {raw}"
        );
        assert!(!raw.contains("secret"), "no directory name survives: {raw}");

        // The ExactPath's extension is retained as a portable matcher; the
        // others become a wildcard glob.
        let fresh = new_project(Some(&template_path)).unwrap();
        let by_id = |id: &str| {
            fresh
                .file
                .sections
                .profiles
                .iter()
                .find(|p| p.id == id)
                .unwrap()
                .matcher
                .clone()
        };
        assert_eq!(
            by_id("exact"),
            ProfileMatch::Extension {
                extension: "csv".into()
            }
        );
        assert_eq!(
            by_id("dir"),
            ProfileMatch::Glob {
                pattern: "*".into()
            }
        );
        assert_eq!(
            by_id("glob"),
            ProfileMatch::Glob {
                pattern: "*".into()
            }
        );
    }

    #[test]
    fn new_project_from_a_full_project_also_strips_sources() {
        // "Use existing project as template": same strip, defensively.
        let dir = tempfile::tempdir().unwrap();
        let (file, _, _) = full_project(dir.path());
        let path = dir.path().join("p.ceesveeproj");
        write_project_file(&path, &file).unwrap();
        let fresh = new_project(Some(&path)).unwrap();
        assert!(fresh.file.sections.sources.is_empty());
        assert!(fresh.file.sections.comparisons.is_empty());
        assert_eq!(fresh.file.sections.profiles.len(), 1);
    }

    // ----- registry -----------------------------------------------------------

    #[test]
    fn every_registered_section_matches_a_field_and_reserved_ones_default_empty() {
        // Guards the registration pattern: a future feature that adds a field
        // without registering it (or vice versa) fails here.
        let sections = serde_json::to_value(ProjectSections::default()).unwrap();
        let map = sections.as_object().unwrap();
        for spec in SECTION_REGISTRY {
            assert!(
                map.contains_key(spec.name),
                "registered section {} has no serialized field",
                spec.name
            );
        }
        // layout serializes as null; everything else defaults to empty lists
        // or objects. Reserved sections are present-but-empty by design.
        for reserved in ["annotations", "dictionary", "queries"] {
            assert_eq!(map[reserved], serde_json::json!([]), "{reserved}");
        }
        assert_eq!(
            map.len(),
            SECTION_REGISTRY.len(),
            "every serialized section is registered (flattened unknowns are empty here)"
        );
    }

    #[test]
    fn atomic_save_survives_replacing_an_existing_project() {
        let dir = tempfile::tempdir().unwrap();
        let (file, _, _) = full_project(dir.path());
        let path = dir.path().join("p.ceesveeproj");
        write_project_file(&path, &file).unwrap();
        let first = std::fs::read(&path).unwrap();

        // A second save over the same path swaps atomically.
        let mut changed = file.clone();
        changed.sections.tabs.active = Some("srcB".into());
        write_project_file(&path, &changed).unwrap();
        let second = std::fs::read(&path).unwrap();
        assert_ne!(first, second);
        let reloaded = load_project_file(&path).unwrap();
        assert_eq!(reloaded.sections.tabs.active.as_deref(), Some("srcB"));
    }
}
