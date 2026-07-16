//! Batch recipes and folder processing (F25): apply a VERSIONED, declarative
//! sequence of existing CEESVEE operations to many files. The step set is
//! closed — reparse, profile validation, filter, transform, deduplicate,
//! column selection, sort, export — there is no scripting, no expressions,
//! no shell, and no network. Files are read only from the explicitly
//! provided input list, outputs go only into the chosen output directory
//! (rendered names are validated against path traversal), and nothing is
//! overwritten unless the run explicitly allows it. A dry run performs no
//! writes at all.

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::dedup::DuplicateKeepStrategy;
use crate::document::Document;
use crate::dto::{ExportOptions, ExportScope, FilterGroup, SortKey};
use crate::error::{AppError, AppResult};
use crate::export;
use crate::filter as filter_mod;
use crate::job::JobCtx;
use crate::parse::{parse, ParseSettings};
use crate::settings::{validate_profile, FileProfile};
use crate::transform::{self, TransformSpec};
use crate::{dedup, encoding, util};

/// The current recipe format version. Loading a different version fails
/// with a clear migration error instead of guessing.
pub const RECIPE_VERSION: u32 = 1;
/// Worker threads are clamped to this.
const MAX_CONCURRENCY: usize = 8;

/// A sort key referencing its column by NAME (recipes outlive layouts).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NamedSortKey {
    pub column: String,
    #[serde(default)]
    pub descending: bool,
}

/// The closed step set. Every variant maps onto an existing engine.
#[derive(Debug, Clone, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RecipeStep {
    /// Parse settings for opening each file (at most one; position ignored).
    Reparse {
        delimiter: Option<String>,
        encoding: Option<String>,
        has_header_row: Option<bool>,
    },
    /// Validate against a saved file profile; optionally fail the file.
    ValidateProfile {
        profile_id: String,
        #[serde(default)]
        fail_on_issues: bool,
    },
    /// Keep only the rows matching the filter.
    Filter { spec: FilterGroup },
    /// Apply a data-cleaning transformation (F06 engine) to the named
    /// columns (empty = every column).
    Transform {
        spec: TransformSpec,
        #[serde(default)]
        columns: Vec<String>,
    },
    /// Remove duplicate rows (F07 engine).
    Deduplicate {
        spec: crate::dedup::DedupSpec,
        keep: DuplicateKeepStrategy,
    },
    /// Keep only the named columns (all must exist).
    SelectColumns { columns: Vec<String> },
    /// Sort by named keys.
    Sort { keys: Vec<NamedSortKey> },
    /// Export the current state to the rendered output path.
    Export { options: ExportOptions },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recipe {
    pub version: u32,
    pub name: String,
    pub steps: Vec<RecipeStep>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchOptions {
    pub recipe: Recipe,
    /// Explicit input files (the UI expands folders before submitting).
    pub files: Vec<String>,
    pub output_dir: String,
    /// Output name template: `{name}` = input stem, `{ext}` = extension.
    pub filename_template: String,
    /// Overwrite existing outputs? Default: never.
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default)]
    pub continue_on_error: bool,
    /// Perform every step but write nothing.
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default = "default_concurrency")]
    pub concurrency: usize,
}

fn default_concurrency() -> usize {
    1
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FileStatus {
    Ok,
    Skipped,
    Failed,
}

/// The exact outcome for one input file.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOutcome {
    pub input: String,
    pub output: Option<String>,
    pub status: FileStatus,
    pub rows_in: usize,
    pub rows_out: usize,
    /// Profile-validation issues found (when a validation step ran).
    pub issues: usize,
    pub steps_applied: usize,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchReport {
    pub recipe_name: String,
    pub dry_run: bool,
    pub ok: usize,
    pub skipped: usize,
    pub failed: usize,
    /// One entry per input file, in input order — nothing is omitted.
    pub outcomes: Vec<FileOutcome>,
}

/// Render the output file name for one input. The result must be a plain
/// file name — separators and parent references are rejected.
pub fn render_name(template: &str, input: &Path) -> AppResult<String> {
    let stem = input
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = input
        .extension()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "csv".to_string());
    let name = template.replace("{name}", &stem).replace("{ext}", &ext);
    if name.trim().is_empty() {
        return Err(AppError::invalid("the output name template is empty"));
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") || name.contains(':') {
        return Err(AppError::invalid(
            "the output name must be a plain file name (no separators or ..)",
        ));
    }
    Ok(name)
}

/// Structural validation before anything runs.
pub fn validate_batch(options: &BatchOptions, profiles: &[FileProfile]) -> AppResult<()> {
    if options.recipe.version != RECIPE_VERSION {
        return Err(AppError::invalid(format!(
            "this recipe is version {}, but this CEESVEE understands version \
             {RECIPE_VERSION} — re-create the recipe",
            options.recipe.version
        )));
    }
    if options.recipe.steps.is_empty() {
        return Err(AppError::invalid("the recipe has no steps"));
    }
    if options.files.is_empty() {
        return Err(AppError::invalid("pick at least one input file"));
    }
    if !options
        .recipe
        .steps
        .iter()
        .any(|s| matches!(s, RecipeStep::Export { .. }))
    {
        return Err(AppError::invalid("the recipe needs an export step"));
    }
    for step in &options.recipe.steps {
        match step {
            RecipeStep::ValidateProfile { profile_id, .. } => {
                if !profiles.iter().any(|p| &p.id == profile_id) {
                    return Err(AppError::invalid(format!(
                        "the recipe references a missing profile ({profile_id})"
                    )));
                }
            }
            RecipeStep::SelectColumns { columns } if columns.is_empty() => {
                return Err(AppError::invalid("select at least one column"));
            }
            RecipeStep::Sort { keys } if keys.is_empty() => {
                return Err(AppError::invalid("the sort step has no keys"));
            }
            _ => {}
        }
    }
    // Every input must render a distinct output name.
    let mut names: Vec<String> = Vec::with_capacity(options.files.len());
    for file in &options.files {
        let name = render_name(&options.filename_template, Path::new(file))?;
        if names.contains(&name) {
            return Err(AppError::invalid(format!(
                "two inputs render the same output name ({name}) — include \
                 {{name}} in the template"
            )));
        }
        names.push(name);
    }
    Ok(())
}

fn column_index(doc: &Document, name: &str) -> AppResult<usize> {
    doc.headers()
        .iter()
        .position(|h| h == name)
        .ok_or_else(|| AppError::invalid(format!("missing column \"{name}\"")))
}

/// Run the recipe against ONE file. Pure except for the final export write.
fn process_file(
    input: &Path,
    options: &BatchOptions,
    profiles: &[FileProfile],
    ctx: &JobCtx,
) -> AppResult<FileOutcome> {
    // Open with the (optional) reparse settings.
    let reparse = options.recipe.steps.iter().find_map(|s| match s {
        RecipeStep::Reparse {
            delimiter,
            encoding,
            has_header_row,
        } => Some((delimiter.clone(), encoding.clone(), *has_header_row)),
        _ => None,
    });
    let bytes = std::fs::read(input)?;
    let settings = ParseSettings {
        delimiter: reparse
            .as_ref()
            .and_then(|(d, _, _)| d.as_deref())
            .map(util::delimiter_to_byte),
        encoding: reparse
            .as_ref()
            .and_then(|(_, e, _)| e.as_deref())
            .map(encoding::from_name),
    };
    let has_header = reparse.as_ref().and_then(|(_, _, h)| *h).unwrap_or(true);
    let parsed = parse(&bytes, &settings)?;
    let mut doc = Document::from_parsed(0, Some(input.to_path_buf()), parsed, has_header);
    let rows_in = doc.n_rows();
    drop(bytes);

    let mut issues = 0usize;
    let mut steps_applied = 0usize;
    let mut output: Option<PathBuf> = None;
    let mut skipped_existing = false;

    for step in &options.recipe.steps {
        ctx.check()?;
        match step {
            RecipeStep::Reparse { .. } => {} // consumed at open
            RecipeStep::ValidateProfile {
                profile_id,
                fail_on_issues,
            } => {
                let profile = profiles
                    .iter()
                    .find(|p| &p.id == profile_id)
                    .ok_or_else(|| AppError::invalid("missing profile"))?;
                let validation = validate_profile(&doc, profile)?;
                issues += validation.issues.len();
                if *fail_on_issues && !validation.ok {
                    return Err(AppError::invalid(format!(
                        "profile validation failed with {} issue(s)",
                        validation.issues.len()
                    )));
                }
            }
            RecipeStep::Filter { spec } => {
                let keep = filter_mod::matching_rows(&doc, spec)?;
                let keep_set: std::collections::HashSet<usize> = keep.into_iter().collect();
                let remove: Vec<usize> = (0..doc.n_rows())
                    .filter(|r| !keep_set.contains(r))
                    .collect();
                if !remove.is_empty() {
                    doc.delete_rows(remove)?;
                }
            }
            RecipeStep::Transform { spec, columns } => {
                let scope = if columns.is_empty() {
                    ExportScope::All
                } else {
                    ExportScope::SelectedColumns {
                        columns: columns
                            .iter()
                            .map(|name| column_index(&doc, name))
                            .collect::<AppResult<_>>()?,
                    }
                };
                let computed = transform::compute(&doc, spec, &scope, None)?;
                transform::commit(&mut doc, computed.changes)?;
            }
            RecipeStep::Deduplicate { spec, keep } => {
                let removals = dedup::removal_rows(&doc, spec, &ExportScope::All, *keep, None)?;
                if !removals.is_empty() {
                    doc.delete_rows(removals)?;
                }
            }
            RecipeStep::SelectColumns { columns } => {
                let keep: Vec<usize> = columns
                    .iter()
                    .map(|name| column_index(&doc, name))
                    .collect::<AppResult<_>>()?;
                let remove: Vec<usize> = (0..doc.n_cols()).filter(|c| !keep.contains(c)).collect();
                if remove.len() == doc.n_cols() {
                    return Err(AppError::invalid("cannot remove every column"));
                }
                if !remove.is_empty() {
                    doc.delete_columns(remove)?;
                }
            }
            RecipeStep::Sort { keys } => {
                let mapped: Vec<SortKey> = keys
                    .iter()
                    .map(|k| {
                        Ok(SortKey {
                            column: column_index(&doc, &k.column)?,
                            descending: k.descending,
                        })
                    })
                    .collect::<AppResult<_>>()?;
                doc.sort(&mapped)?;
            }
            RecipeStep::Export {
                options: export_options,
            } => {
                let name = render_name(&options.filename_template, input)?;
                let dest = Path::new(&options.output_dir).join(&name);
                if dest.exists() && !options.overwrite {
                    skipped_existing = true;
                    output = Some(dest);
                    steps_applied += 1;
                    continue;
                }
                output = Some(dest.clone());
                if !options.dry_run {
                    // Stage-and-rename so a cancelled/failed write never
                    // leaves a half-written output behind. The staging name
                    // APPENDS to the full output name ("a.csv.ceesvee-partial")
                    // — with_extension would collapse "a.csv" and "a.tsv"
                    // onto one staging path, letting parallel workers
                    // truncate or steal each other's temp file.
                    let staging = {
                        let mut name = dest
                            .file_name()
                            .map(|n| n.to_os_string())
                            .unwrap_or_default();
                        name.push(".ceesvee-partial");
                        dest.with_file_name(name)
                    };
                    let result = (|| -> AppResult<()> {
                        let file = std::fs::File::create(&staging)?;
                        let mut writer = std::io::BufWriter::new(file);
                        export::write_document(&doc, export_options, &mut writer, Some(ctx))?;
                        writer.flush()?;
                        drop(writer);
                        std::fs::rename(&staging, &dest)?;
                        Ok(())
                    })();
                    if let Err(e) = result {
                        let _ = std::fs::remove_file(&staging);
                        return Err(e);
                    }
                }
            }
        }
        steps_applied += 1;
    }

    Ok(FileOutcome {
        input: input.display().to_string(),
        output: output.map(|p| p.display().to_string()),
        status: if skipped_existing {
            FileStatus::Skipped
        } else {
            FileStatus::Ok
        },
        rows_in,
        rows_out: doc.n_rows(),
        issues,
        steps_applied,
        error: if skipped_existing {
            Some("output already exists (overwrite is off)".to_string())
        } else {
            None
        },
    })
}

/// Run the whole batch. One cancellable job; `concurrency` worker threads
/// pull files from a shared queue. The report covers EVERY input file.
pub fn run_batch(
    options: &BatchOptions,
    profiles: &[FileProfile],
    ctx: &JobCtx,
) -> AppResult<BatchReport> {
    validate_batch(options, profiles)?;
    if !options.dry_run {
        std::fs::create_dir_all(&options.output_dir)?;
    }
    ctx.set_total(options.files.len() as u64);

    let queue: Mutex<VecDeque<(usize, PathBuf)>> = Mutex::new(
        options
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (i, PathBuf::from(f)))
            .collect(),
    );
    let outcomes: Mutex<Vec<Option<FileOutcome>>> = Mutex::new(vec![None; options.files.len()]);
    let fatal: Mutex<Option<AppError>> = Mutex::new(None);
    let workers = options.concurrency.clamp(1, MAX_CONCURRENCY);

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| loop {
                if ctx.is_cancelled() || fatal.lock().map(|f| f.is_some()).unwrap_or(true) {
                    return;
                }
                let next = queue.lock().ok().and_then(|mut q| q.pop_front());
                let Some((index, path)) = next else { return };
                ctx.set_message(format!("processing {}", path.display()));
                let outcome = match process_file(&path, options, profiles, ctx) {
                    Ok(outcome) => outcome,
                    Err(AppError::Cancelled) => return,
                    Err(e) if options.continue_on_error => FileOutcome {
                        input: path.display().to_string(),
                        output: None,
                        status: FileStatus::Failed,
                        rows_in: 0,
                        rows_out: 0,
                        issues: 0,
                        steps_applied: 0,
                        error: Some(e.to_string()),
                    },
                    Err(e) => {
                        if let Ok(mut fatal) = fatal.lock() {
                            fatal.get_or_insert(e);
                        }
                        return;
                    }
                };
                if let Ok(mut slots) = outcomes.lock() {
                    slots[index] = Some(outcome);
                }
                let _ = ctx.advance(1);
            });
        }
    });

    ctx.check()?;
    if let Ok(mut fatal) = fatal.lock() {
        if let Some(e) = fatal.take() {
            return Err(e);
        }
    }

    let outcomes: Vec<FileOutcome> = outcomes
        .into_inner()
        .map_err(|_| AppError::Other("internal batch state error".into()))?
        .into_iter()
        .enumerate()
        .map(|(i, o)| {
            o.unwrap_or_else(|| FileOutcome {
                input: options.files[i].clone(),
                output: None,
                status: FileStatus::Skipped,
                rows_in: 0,
                rows_out: 0,
                issues: 0,
                steps_applied: 0,
                error: Some("not processed (the batch stopped early)".to_string()),
            })
        })
        .collect();

    Ok(BatchReport {
        recipe_name: options.recipe.name.clone(),
        dry_run: options.dry_run,
        ok: outcomes
            .iter()
            .filter(|o| o.status == FileStatus::Ok)
            .count(),
        skipped: outcomes
            .iter()
            .filter(|o| o.status == FileStatus::Skipped)
            .count(),
        failed: outcomes
            .iter()
            .filter(|o| o.status == FileStatus::Failed)
            .count(),
        outcomes,
    })
}

/// Finished batch reports, keyed by job id (pruned opportunistically: a
/// bounded number of recent reports is kept).
#[derive(Default)]
pub struct RecipeCache(std::sync::Arc<Mutex<std::collections::HashMap<u64, BatchReport>>>);

impl RecipeCache {
    pub fn share(&self) -> std::sync::Arc<Mutex<std::collections::HashMap<u64, BatchReport>>> {
        std::sync::Arc::clone(&self.0)
    }

    pub fn get(&self, job_id: u64) -> Option<BatchReport> {
        self.0.lock().ok()?.get(&job_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobRegistry;

    fn write_input(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    fn export_options() -> ExportOptions {
        serde_json::from_value(serde_json::json!({
            "delimiter": ",",
            "encoding": "UTF-8",
            "quoteStyle": "minimal",
            "lineEnding": "lf",
            "bom": false,
        }))
        .unwrap()
    }

    fn batch(files: Vec<String>, output_dir: &Path, steps: Vec<RecipeStep>) -> BatchOptions {
        BatchOptions {
            recipe: Recipe {
                version: RECIPE_VERSION,
                name: "test".into(),
                steps,
            },
            files,
            output_dir: output_dir.display().to_string(),
            filename_template: "{name}_out.{ext}".into(),
            overwrite: false,
            continue_on_error: false,
            dry_run: false,
            concurrency: 2,
        }
    }

    fn run(options: &BatchOptions) -> AppResult<BatchReport> {
        let registry = JobRegistry::default();
        let ctx = registry.begin("batch", None, |_| {});
        run_batch(options, &[], &ctx)
    }

    #[test]
    fn deterministic_outputs_and_full_report() {
        let dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let a = write_input(dir.path(), "a.csv", "n,v\n2,x\n1,y\n");
        let b = write_input(dir.path(), "b.csv", "n,v\n9,z\n");
        let options = batch(
            vec![a.display().to_string(), b.display().to_string()],
            out.path(),
            vec![
                RecipeStep::Sort {
                    keys: vec![NamedSortKey {
                        column: "n".into(),
                        descending: false,
                    }],
                },
                RecipeStep::Export {
                    options: export_options(),
                },
            ],
        );
        let report = run(&options).unwrap();
        assert_eq!(report.ok, 2);
        assert_eq!(report.outcomes.len(), 2, "every input is reported");
        let written = std::fs::read_to_string(out.path().join("a_out.csv")).unwrap();
        assert_eq!(written, "n,v\n1,y\n2,x\n");

        // Same inputs + recipe -> identical outputs.
        let mut again = options.clone();
        again.overwrite = true;
        let _ = run(&again).unwrap();
        let rewritten = std::fs::read_to_string(out.path().join("a_out.csv")).unwrap();
        assert_eq!(written, rewritten);
    }

    #[test]
    fn dry_run_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let a = write_input(dir.path(), "a.csv", "n\n1\n");
        let mut options = batch(
            vec![a.display().to_string()],
            out.path(),
            vec![RecipeStep::Export {
                options: export_options(),
            }],
        );
        options.dry_run = true;
        let report = run(&options).unwrap();
        assert!(report.dry_run);
        assert_eq!(report.ok, 1);
        assert!(
            std::fs::read_dir(out.path()).unwrap().next().is_none(),
            "no output files were written"
        );
    }

    #[test]
    fn no_overwrite_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let a = write_input(dir.path(), "a.csv", "n\n1\n");
        std::fs::write(out.path().join("a_out.csv"), "existing").unwrap();
        let options = batch(
            vec![a.display().to_string()],
            out.path(),
            vec![RecipeStep::Export {
                options: export_options(),
            }],
        );
        let report = run(&options).unwrap();
        assert_eq!(report.skipped, 1);
        assert_eq!(
            std::fs::read_to_string(out.path().join("a_out.csv")).unwrap(),
            "existing",
            "the existing file is untouched"
        );
    }

    #[test]
    fn one_failure_is_isolated_under_continue_on_error() {
        let dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let good = write_input(dir.path(), "good.csv", "n\n1\n");
        let missing = dir.path().join("missing.csv");
        let mut options = batch(
            vec![good.display().to_string(), missing.display().to_string()],
            out.path(),
            vec![RecipeStep::Export {
                options: export_options(),
            }],
        );
        options.continue_on_error = true;
        let report = run(&options).unwrap();
        assert_eq!(report.ok, 1);
        assert_eq!(report.failed, 1);
        assert!(report.outcomes[1].error.is_some());
        assert!(out.path().join("good_out.csv").exists());

        // Stop-on-error: the whole batch fails instead.
        options.continue_on_error = false;
        assert!(run(&options).is_err() || run(&options).unwrap().failed > 0);
    }

    #[test]
    fn filter_transform_select_and_dedup_steps_compose() {
        let dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let a = write_input(
            dir.path(),
            "a.csv",
            "id,name,junk\n1,  ann  ,x\n1,  ann  ,y\n2,bob,z\n",
        );
        let steps = vec![
            RecipeStep::Transform {
                spec: serde_json::from_value(serde_json::json!({"type": "trim"})).unwrap(),
                columns: vec!["name".into()],
            },
            RecipeStep::SelectColumns {
                columns: vec!["id".into(), "name".into()],
            },
            RecipeStep::Deduplicate {
                spec: serde_json::from_value(serde_json::json!({
                    "keyColumns": [0, 1],
                }))
                .unwrap(),
                keep: serde_json::from_value(serde_json::json!("first")).unwrap(),
            },
            RecipeStep::Export {
                options: export_options(),
            },
        ];
        let options = batch(vec![a.display().to_string()], out.path(), steps);
        let report = run(&options).unwrap();
        assert_eq!(report.ok, 1);
        let written = std::fs::read_to_string(out.path().join("a_out.csv")).unwrap();
        assert_eq!(written, "id,name\n1,ann\n2,bob\n");
        assert_eq!(report.outcomes[0].rows_in, 3);
        assert_eq!(report.outcomes[0].rows_out, 2);
    }

    #[test]
    fn version_mismatch_and_bad_templates_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let a = write_input(dir.path(), "a.csv", "n\n1\n");
        let mut options = batch(
            vec![a.display().to_string()],
            out.path(),
            vec![RecipeStep::Export {
                options: export_options(),
            }],
        );
        options.recipe.version = 2;
        let err = match run(&options) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("version mismatch must fail"),
        };
        assert!(err.contains("version"));

        options.recipe.version = RECIPE_VERSION;
        options.filename_template = "../{name}.csv".into();
        assert!(run(&options).is_err(), "path traversal rejected");

        options.filename_template = "static.csv".into();
        options.files.push(a.display().to_string());
        assert!(run(&options).is_err(), "duplicate output names rejected");
    }

    #[test]
    fn missing_columns_fail_the_file_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let a = write_input(dir.path(), "a.csv", "n\n1\n");
        let options = batch(
            vec![a.display().to_string()],
            out.path(),
            vec![
                RecipeStep::SelectColumns {
                    columns: vec!["nope".into()],
                },
                RecipeStep::Export {
                    options: export_options(),
                },
            ],
        );
        let err = match run(&options) {
            Err(e) => e.to_string(),
            Ok(_) => panic!("missing column must fail"),
        };
        assert!(err.contains("nope"));
    }
}
