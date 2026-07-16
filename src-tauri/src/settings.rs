//! Persisted application settings (F08): named file profiles plus safe UI
//! preferences, stored as versioned JSON in the Tauri application-data
//! directory. Never contains document data (cell contents, samples, copied
//! values) — only configuration.
//!
//! Corrupt or unreadable settings fail safely: the broken file is preserved
//! as a `.corrupt` backup and defaults are returned, so one bad write can
//! never brick the app or silently discard the user's profiles.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::document::Document;
use crate::dto::BackupPolicy;
use crate::error::{AppError, AppResult};
use crate::{analyze, save};

pub const SETTINGS_FILE: &str = "settings.json";
pub const SETTINGS_VERSION: u32 = 1;

/// How a profile decides whether it applies to a file path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ProfileMatch {
    ExactPath { path: String },
    Directory { directory: String },
    Extension { extension: String },
    Glob { pattern: String },
}

/// Expected data type for a named column, checked by profile validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExpectedType {
    Number,
    Date,
    Bool,
    Text,
}

/// A regex a named column's non-blank values must fully match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegexRule {
    pub column: String,
    pub pattern: String,
}

/// Numeric bounds for a named column (dates can be validated by regex/type).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RangeRule {
    pub column: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// One key of a named view's non-destructive sort (F12), by stable column ID
/// so it survives renames AND reorders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewSortKey {
    pub column_id: String,
    #[serde(default)]
    pub descending: bool,
}

/// A named, reusable, NON-DESTRUCTIVE way of looking at a matching document
/// (F12): row filter + view sort + column layout. Columns are referenced by
/// stable logical IDs (`DocumentMeta::column_ids`); the filter keeps its
/// column indices but carries the ID snapshot they were saved against, so the
/// front end can remap (or warn recoverably) when the structure changed.
/// Applying a view never mutates data and never marks a document dirty.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NamedView {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub filter: Option<crate::dto::FilterGroup>,
    /// Column IDs at save time, positionally aligned with the document the
    /// filter's indices refer to (used to remap after structural edits).
    #[serde(default)]
    pub filter_column_ids: Vec<String>,
    #[serde(default)]
    pub sort_keys: Vec<ViewSortKey>,
    #[serde(default)]
    pub hidden_column_ids: Vec<String>,
    /// Arbitrary pinned columns (not just a leading count), in pin order.
    #[serde(default)]
    pub pinned_column_ids: Vec<String>,
    /// Display order for unpinned columns; IDs not listed keep file order.
    #[serde(default)]
    pub column_order: Vec<String>,
    /// Column widths in px, keyed by column ID.
    #[serde(default)]
    pub column_widths: std::collections::HashMap<String, f64>,
    #[serde(default)]
    pub wrap_text: bool,
}

/// A reusable description of a recurring file format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileProfile {
    pub id: String,
    pub name: String,
    pub matcher: ProfileMatch,
    /// Whether a matching file is reparsed with these settings automatically
    /// (clean documents only — a dirty document is never silently reparsed).
    #[serde(default)]
    pub auto_apply: bool,

    // Parse settings.
    pub delimiter: Option<String>,
    pub encoding: Option<String>,
    pub has_header_row: Option<bool>,

    /// Default export options offered when exporting a matching document.
    #[serde(default)]
    pub default_export: Option<crate::dto::ExportOptions>,

    // Validation rules (all optional).
    /// Expected column names; with `enforce_order`, also their order.
    #[serde(default)]
    pub expected_columns: Vec<String>,
    #[serde(default)]
    pub enforce_order: bool,
    #[serde(default)]
    pub expected_types: Vec<(String, ExpectedType)>,
    #[serde(default)]
    pub required_columns: Vec<String>,
    #[serde(default)]
    pub unique_columns: Vec<String>,
    #[serde(default)]
    pub regex_rules: Vec<RegexRule>,
    #[serde(default)]
    pub range_rules: Vec<RangeRule>,

    /// F26: user overrides of detected semantic types, keyed by column name
    /// so they survive rescans and reopened files. `FreeText` forces a column
    /// back to plain text regardless of what detection says.
    #[serde(default)]
    pub semantic_types: Vec<(String, crate::semantic::SemanticType)>,

    /// F27: cross-column validation rules (closed DTO set, columns by name).
    #[serde(default)]
    pub cross_rules: Vec<crate::crossval::CrossRule>,

    /// F12: named views saved for matching files.
    #[serde(default)]
    pub named_views: Vec<NamedView>,
    /// F12: the view last applied to a matching file, restored on reopen.
    #[serde(default)]
    pub last_view_id: Option<String>,
}

/// The persisted settings document.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub version: u32,
    #[serde(default)]
    pub profiles: Vec<FileProfile>,
    /// F11: user shortcut overrides, keyed by stable command id. A `null`
    /// value unbinds the command; a missing key keeps its default shortcut.
    /// Bindings use the normalized `mod+shift+k` syntax owned by the front
    /// end — the backend only persists them.
    #[serde(default)]
    pub shortcut_overrides: std::collections::HashMap<String, Option<String>>,
    /// F16: OPT-IN crash-recovery journaling (journals may contain edited
    /// cell values; the UI shows a privacy disclosure).
    #[serde(default)]
    pub recovery_enabled: bool,
    /// F16: journals older than this are swept at startup.
    #[serde(default = "default_recovery_retention")]
    pub recovery_retention_days: u32,
}

fn default_recovery_retention() -> u32 {
    7
}

impl Default for AppSettings {
    fn default() -> Self {
        AppSettings {
            version: SETTINGS_VERSION,
            profiles: Vec::new(),
            shortcut_overrides: std::collections::HashMap::new(),
            recovery_enabled: false,
            recovery_retention_days: default_recovery_retention(),
        }
    }
}

/// Load settings from `dir`, returning defaults when the file is missing.
/// A file that exists but cannot be parsed is moved aside to
/// `settings.json.corrupt` (preserving the user's data for manual recovery)
/// and defaults are returned.
pub fn load_settings(dir: &Path) -> AppSettings {
    let path = dir.join(SETTINGS_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return AppSettings::default(),
    };
    match serde_json::from_slice::<AppSettings>(&bytes) {
        Ok(settings) => settings,
        Err(_) => {
            let backup = corrupt_backup_path(&path);
            let _ = std::fs::rename(&path, backup);
            AppSettings::default()
        }
    }
}

/// Persist settings atomically (via the F03 staging + swap pipeline).
pub fn save_settings(dir: &Path, settings: &AppSettings) -> AppResult<()> {
    std::fs::create_dir_all(dir)?;
    let json = serde_json::to_vec_pretty(settings)
        .map_err(|e| AppError::Other(format!("settings serialization failed: {e}")))?;
    let path = dir.join(SETTINGS_FILE);
    save::atomic_write(&path, BackupPolicy::None, |file| {
        use std::io::Write;
        file.write_all(&json)?;
        Ok(json.len() as u64)
    })?;
    Ok(())
}

fn corrupt_backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".corrupt");
    path.with_file_name(name)
}

// ----- validation ---------------------------------------------------------------

/// One violated rule, with enough context to render and (roughly) locate it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileIssue {
    pub kind: String,
    pub column: Option<String>,
    pub detail: String,
    pub affected_count: usize,
}

/// The outcome of checking a document against a profile's expectations.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileValidation {
    pub profile_id: String,
    pub ok: bool,
    pub issues: Vec<ProfileIssue>,
}

/// Check `doc` against `profile`'s column and data rules. Read-only.
pub fn validate_profile(doc: &Document, profile: &FileProfile) -> AppResult<ProfileValidation> {
    let mut issues: Vec<ProfileIssue> = Vec::new();
    let headers = doc.headers();
    let col_index = |name: &str| headers.iter().position(|h| h == name);

    // Missing / extra / misordered columns.
    if !profile.expected_columns.is_empty() {
        for expected in &profile.expected_columns {
            if col_index(expected).is_none() {
                issues.push(ProfileIssue {
                    kind: "missingColumn".into(),
                    column: Some(expected.clone()),
                    detail: format!("expected column “{expected}” is missing"),
                    affected_count: 1,
                });
            }
        }
        for (i, header) in headers.iter().enumerate() {
            if !profile.expected_columns.iter().any(|e| e == header) {
                issues.push(ProfileIssue {
                    kind: "extraColumn".into(),
                    column: Some(header.clone()),
                    detail: format!("column {} (“{header}”) is not in the profile", i + 1),
                    affected_count: 1,
                });
            }
        }
        if profile.enforce_order {
            let actual_order: Vec<&String> = headers
                .iter()
                .filter(|h| profile.expected_columns.contains(h))
                .collect();
            let expected_order: Vec<&String> = profile
                .expected_columns
                .iter()
                .filter(|e| col_index(e).is_some())
                .collect();
            if actual_order != expected_order {
                issues.push(ProfileIssue {
                    kind: "columnOrder".into(),
                    column: None,
                    detail: "columns are not in the profile's expected order".into(),
                    affected_count: 1,
                });
            }
        }
    }

    for required in &profile.required_columns {
        let Some(c) = col_index(required) else {
            // Missing entirely — covered above when listed in expected_columns;
            // still report so required-only profiles get a clear signal.
            issues.push(ProfileIssue {
                kind: "missingColumn".into(),
                column: Some(required.clone()),
                detail: format!("required column “{required}” is missing"),
                affected_count: 1,
            });
            continue;
        };
        let mut blanks = 0usize;
        doc.visit_rows(0..doc.n_rows(), &mut |_, row| {
            if row[c].trim().is_empty() {
                blanks += 1;
            }
            Ok(true)
        })?;
        if blanks > 0 {
            issues.push(ProfileIssue {
                kind: "requiredBlank".into(),
                column: Some(required.clone()),
                detail: format!("“{required}” has {blanks} blank cell(s) but is required"),
                affected_count: blanks,
            });
        }
    }

    for unique in &profile.unique_columns {
        let Some(c) = col_index(unique) else { continue };
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut dupes = 0usize;
        doc.visit_rows(0..doc.n_rows(), &mut |_, row| {
            if seen.contains(&row[c]) {
                dupes += 1;
            } else {
                seen.insert(row[c].clone());
            }
            Ok(true)
        })?;
        if dupes > 0 {
            issues.push(ProfileIssue {
                kind: "nonUnique".into(),
                column: Some(unique.clone()),
                detail: format!("“{unique}” has {dupes} duplicated value(s) but must be unique"),
                affected_count: dupes,
            });
        }
    }

    for (name, expected) in &profile.expected_types {
        let Some(c) = col_index(name) else { continue };
        let mut bad = 0usize;
        doc.visit_rows(0..doc.n_rows(), &mut |_, row| {
            let cell = row[c].trim();
            if !cell.is_empty() {
                let ok = match expected {
                    ExpectedType::Number => analyze::as_number(cell).is_some(),
                    ExpectedType::Date => analyze::is_date(cell),
                    ExpectedType::Bool => analyze::is_bool(cell),
                    ExpectedType::Text => true,
                };
                if !ok {
                    bad += 1;
                }
            }
            Ok(true)
        })?;
        if bad > 0 {
            issues.push(ProfileIssue {
                kind: "typeMismatch".into(),
                column: Some(name.clone()),
                detail: format!("“{name}” has {bad} cell(s) that are not {expected:?}"),
                affected_count: bad,
            });
        }
    }

    for rule in &profile.regex_rules {
        let Some(c) = col_index(&rule.column) else {
            continue;
        };
        // The rule requires the WHOLE (trimmed) value to match, so anchor the
        // user's pattern; a bare `is_match` would accept any substring hit
        // (e.g. `\d{5}` passing "abc12345xyz").
        let re = regex::Regex::new(&format!("^(?:{})$", rule.pattern))
            .map_err(|e| AppError::invalid(format!("profile regex is invalid: {e}")))?;
        let mut bad = 0usize;
        doc.visit_rows(0..doc.n_rows(), &mut |_, row| {
            let cell = row[c].trim();
            if !cell.is_empty() && !re.is_match(cell) {
                bad += 1;
            }
            Ok(true)
        })?;
        if bad > 0 {
            issues.push(ProfileIssue {
                kind: "regexMismatch".into(),
                column: Some(rule.column.clone()),
                detail: format!(
                    "“{}” has {bad} cell(s) not matching {}",
                    rule.column, rule.pattern
                ),
                affected_count: bad,
            });
        }
    }

    for rule in &profile.range_rules {
        let Some(c) = col_index(&rule.column) else {
            continue;
        };
        let mut bad = 0usize;
        doc.visit_rows(0..doc.n_rows(), &mut |_, row| {
            let cell = row[c].trim();
            if !cell.is_empty() {
                match analyze::as_number(cell) {
                    Some(n) => {
                        if rule.min.is_some_and(|min| n < min)
                            || rule.max.is_some_and(|max| n > max)
                        {
                            bad += 1;
                        }
                    }
                    None => bad += 1,
                }
            }
            Ok(true)
        })?;
        if bad > 0 {
            issues.push(ProfileIssue {
                kind: "outOfRange".into(),
                column: Some(rule.column.clone()),
                detail: format!(
                    "“{}” has {bad} cell(s) outside the allowed range",
                    rule.column
                ),
                affected_count: bad,
            });
        }
    }

    Ok(ProfileValidation {
        profile_id: profile.id.clone(),
        ok: issues.is_empty(),
        issues,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse, ParseSettings};

    fn doc_from(csv: &str) -> Document {
        let parsed = parse(csv.as_bytes(), &ParseSettings::default()).unwrap();
        Document::from_parsed(1, None, parsed, true)
    }

    fn profile() -> FileProfile {
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
            expected_columns: vec!["id".into(), "amount".into(), "email".into()],
            enforce_order: true,
            expected_types: vec![("amount".into(), ExpectedType::Number)],
            required_columns: vec!["id".into()],
            unique_columns: vec!["id".into()],
            regex_rules: vec![RegexRule {
                column: "email".into(),
                pattern: "^[^@]+@[^@]+$".into(),
            }],
            range_rules: vec![RangeRule {
                column: "amount".into(),
                min: Some(0.0),
                max: Some(1000.0),
            }],
            semantic_types: Vec::new(),
            cross_rules: Vec::new(),
            named_views: Vec::new(),
            last_view_id: None,
        }
    }

    #[test]
    fn named_views_round_trip_and_default_empty() {
        use crate::dto::{Conjunction, FilterCondition, FilterGroup, FilterNode, FilterOp};
        let dir = tempfile::tempdir().unwrap();
        let mut settings = AppSettings::default();
        let mut p = profile();
        assert!(p.named_views.is_empty());
        assert!(p.last_view_id.is_none());

        p.named_views = vec![NamedView {
            id: "v1".into(),
            name: "QA slice".into(),
            filter: Some(FilterGroup {
                conjunction: Conjunction::And,
                nodes: vec![FilterNode::Condition(FilterCondition {
                    column: 1,
                    op: FilterOp::NotEmpty,
                    value: String::new(),
                    case_sensitive: false,
                })],
            }),
            filter_column_ids: vec!["c0".into(), "c1".into()],
            sort_keys: vec![ViewSortKey {
                column_id: "c1".into(),
                descending: true,
            }],
            hidden_column_ids: vec!["c0".into()],
            pinned_column_ids: vec!["c2".into()],
            column_order: vec!["c2".into(), "c1".into()],
            column_widths: std::collections::HashMap::from([("c1".to_string(), 240.0)]),
            wrap_text: true,
        }];
        p.last_view_id = Some("v1".into());
        settings.profiles.push(p);
        save_settings(dir.path(), &settings).unwrap();
        let loaded = load_settings(dir.path());
        assert_eq!(
            loaded.profiles[0].named_views,
            settings.profiles[0].named_views
        );
        assert_eq!(loaded.profiles[0].last_view_id, Some("v1".to_string()));
    }

    #[test]
    fn cross_rules_round_trip_and_default_empty() {
        use crate::crossval::CrossRule;
        let dir = tempfile::tempdir().unwrap();
        let mut settings = AppSettings::default();
        let mut p = profile();
        assert!(p.cross_rules.is_empty());
        p.cross_rules = vec![CrossRule::ColumnsEqual {
            left: "a".into(),
            right: "b".into(),
            negate: true,
        }];
        settings.profiles.push(p);
        save_settings(dir.path(), &settings).unwrap();
        let loaded = load_settings(dir.path());
        assert_eq!(
            loaded.profiles[0].cross_rules,
            settings.profiles[0].cross_rules
        );
    }

    #[test]
    fn semantic_overrides_round_trip_and_default_empty() {
        use crate::semantic::SemanticType;
        let dir = tempfile::tempdir().unwrap();
        // A pre-F26 profile (no semanticTypes key) still loads, empty.
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            br#"{"version":1,"profiles":[{"id":"p","name":"n",
                 "matcher":{"type":"extension","extension":"csv"},
                 "delimiter":null,"encoding":null,"hasHeaderRow":null}]}"#,
        )
        .unwrap();
        let mut settings = load_settings(dir.path());
        assert!(settings.profiles[0].semantic_types.is_empty());

        // Overrides persist across save/load (they survive rescans by
        // keying on the column NAME, not its position).
        settings.profiles[0].semantic_types = vec![
            ("email".into(), SemanticType::Email),
            ("notes".into(), SemanticType::FreeText),
        ];
        save_settings(dir.path(), &settings).unwrap();
        let loaded = load_settings(dir.path());
        assert_eq!(
            loaded.profiles[0].semantic_types,
            vec![
                ("email".into(), SemanticType::Email),
                ("notes".into(), SemanticType::FreeText),
            ]
        );
    }

    #[test]
    fn valid_document_passes() {
        let d = doc_from("id,amount,email\n1,10,a@b.c\n2,20,x@y.z");
        let v = validate_profile(&d, &profile()).unwrap();
        assert!(v.ok, "{:?}", v.issues);
    }

    #[test]
    fn reports_missing_extra_and_misordered_columns() {
        let d = doc_from("amount,id,bonus\n10,1,x");
        let v = validate_profile(&d, &profile()).unwrap();
        let kinds: Vec<&str> = v.issues.iter().map(|i| i.kind.as_str()).collect();
        assert!(kinds.contains(&"missingColumn"), "email is missing");
        assert!(kinds.contains(&"extraColumn"), "bonus is extra");
        assert!(kinds.contains(&"columnOrder"), "amount/id are swapped");
    }

    #[test]
    fn reports_blank_required_nonunique_type_regex_and_range() {
        let d = doc_from("id,amount,email\n1,10,a@b.c\n1,oops,bad-email\n,2000,x@y.z");
        let v = validate_profile(&d, &profile()).unwrap();
        let kind = |k: &str| v.issues.iter().find(|i| i.kind == k);
        assert!(kind("requiredBlank").is_some(), "blank id");
        assert!(kind("nonUnique").is_some(), "duplicate id 1");
        assert_eq!(kind("typeMismatch").unwrap().affected_count, 1, "oops");
        assert!(kind("regexMismatch").is_some(), "bad-email");
        // 2000 exceeds max AND "oops" is non-numeric: both out of range.
        assert_eq!(kind("outOfRange").unwrap().affected_count, 2);
    }

    #[test]
    fn shortcut_overrides_round_trip_and_default_empty() {
        let dir = tempfile::tempdir().unwrap();
        // A pre-F11 settings file (no shortcutOverrides key) still loads.
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            br#"{"version":1,"profiles":[]}"#,
        )
        .unwrap();
        let loaded = load_settings(dir.path());
        assert!(loaded.shortcut_overrides.is_empty());

        // Overrides persist, including explicit unbinds (null).
        let mut settings = AppSettings::default();
        settings
            .shortcut_overrides
            .insert("file.save".into(), Some("mod+shift+s".into()));
        settings.shortcut_overrides.insert("edit.redo".into(), None);
        save_settings(dir.path(), &settings).unwrap();
        let loaded = load_settings(dir.path());
        assert_eq!(
            loaded.shortcut_overrides.get("file.save"),
            Some(&Some("mod+shift+s".to_string()))
        );
        assert_eq!(loaded.shortcut_overrides.get("edit.redo"), Some(&None));
    }

    #[test]
    fn regex_rules_require_a_full_match_not_a_substring() {
        // An unanchored pattern must still validate the WHOLE value: "12345"
        // embedded in surrounding text is a mismatch, not a pass.
        let mut p = profile();
        p.regex_rules = vec![RegexRule {
            column: "email".into(),
            pattern: r"\d{5}".into(),
        }];
        let d = doc_from("id,amount,email\n1,10,12345\n2,20,abc12345xyz");
        let v = validate_profile(&d, &p).unwrap();
        let issue = v.issues.iter().find(|i| i.kind == "regexMismatch");
        assert_eq!(
            issue.map(|i| i.affected_count),
            Some(1),
            "only the embedded value fails"
        );
    }

    #[test]
    fn invalid_profile_regex_is_reported_not_panicked() {
        let mut p = profile();
        p.regex_rules[0].pattern = "([".into();
        let d = doc_from("id,amount,email\n1,10,a@b.c");
        assert!(validate_profile(&d, &p).is_err());
    }

    #[test]
    fn settings_round_trip_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        // Missing file -> defaults.
        let loaded = load_settings(dir.path());
        assert_eq!(loaded.version, SETTINGS_VERSION);
        assert!(loaded.profiles.is_empty());

        let mut settings = AppSettings::default();
        settings.profiles.push(profile());
        save_settings(dir.path(), &settings).unwrap();
        let loaded = load_settings(dir.path());
        assert_eq!(loaded.profiles.len(), 1);
        assert_eq!(loaded.profiles[0].name, "orders");
    }

    #[test]
    fn corrupt_settings_fail_safely_and_preserve_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SETTINGS_FILE);
        std::fs::write(&path, b"{ not json !!!").unwrap();

        let loaded = load_settings(dir.path());
        assert!(loaded.profiles.is_empty(), "defaults on corruption");
        assert!(!path.exists(), "corrupt file moved aside");
        let backup = dir.path().join(format!("{SETTINGS_FILE}.corrupt"));
        assert_eq!(std::fs::read(&backup).unwrap(), b"{ not json !!!");
    }

    #[test]
    fn settings_json_contains_no_cell_data_fields() {
        // The persisted schema carries configuration only; spot-check the
        // serialized form of a full profile for data-bearing fields.
        let mut settings = AppSettings::default();
        settings.profiles.push(profile());
        let json = serde_json::to_string(&settings).unwrap();
        for forbidden in ["cells", "rows", "samples", "values", "clipboard"] {
            assert!(
                !json.contains(&format!("\"{forbidden}\"")),
                "settings JSON must not contain {forbidden}"
            );
        }
    }
}
