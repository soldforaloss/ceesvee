//! CEESVEE core library: a Rust-owned, in-memory CSV/delimited-file model
//! exposed to the web front end through a small Tauri command surface.

mod analyze;
/// Public like [`job`]: the F40 annotations engine (row bookmarks, tags and
/// notes anchored by `row_identity` handles, the rematch engine, filter
/// predicates, tag-to-column and versioned sidecar/project-section persistence)
/// is a stable internal API consumed by the command surface and test harness.
pub mod annotations;
mod append;
mod archive;
mod clipboard;
mod cluster;
mod commands;
mod compare;
mod crossval;
mod dedup;
mod delimiter;
mod derived;
mod diagnostics;
mod dialect;
/// Public like [`job`]: the F38 data-dictionary model (per-column
/// documentation keyed by stable column ID, versioned import/export, the merge
/// engine and the profile/PII integration hooks) is a stable internal API
/// consumed by the profile and PII modules and the test harness.
pub mod dictionary;
mod document;
mod dto;
mod encoding;
mod error;
mod export;
mod export_scope;
mod filter;
mod find;
mod follow;
mod groupby;
mod index;
/// Public so downstream features (and the test harness) can treat the job
/// registry, progress plumbing and cancellation as a stable internal API.
pub mod job;
mod joins;
mod journal;
mod json_export;
/// Public like [`job`]: the F33 JSON / JSON Lines import engine (shape
/// detection, streaming JSON Pointer resolution, the JSONL byte-offset
/// record index, flatten/explode policies, preview scan and the
/// document-producing import) consumed by the F33 command surface and the
/// JSON export stage.
pub mod json_import;
mod outlier;
mod parse;
mod paste;
mod pii;
mod profile;
mod project;
mod recipe;
mod reopen;
mod repair;
mod reshape;
/// Public like [`job`]: the shared row-identity model (editor row ids,
/// source record numbers, normalized composite keys, content hashes and the
/// key→row resolver) consumed by F40 annotations, F46 patches and F47
/// three-way merge.
pub mod row_identity;
/// Reproducible sampling & partitioning (F48): seeded PRNG, the eight sampling
/// methods, weighted/stratified/group-preserving partitioning, previews, and
/// manifested execution over the shared [`tabular`] contracts.
mod sampling;
mod save;
/// Public like [`job`]: the F31 schema core (logical types, classification,
/// typed parsing, inference, import/export) is a stable internal API consumed
/// by later feature stages and the test harness.
pub mod schema;
mod schema_ops;
mod semantic;
mod settings;
mod sort;
mod state;
/// Public like [`job`]: the shared TabularSource/TabularSink contracts
/// (schema + windowed reads + fingerprints; atomic streamed writes) that
/// upcoming import/export/sampling features implement and consume.
pub mod tabular;
mod transform;
mod util;

use std::sync::Mutex;

use crate::compare::CompareCache;
use crate::dedup::DedupCache;
use crate::diagnostics::DiagnosticsCache;
use crate::job::JobRegistry;
use crate::profile::ProfileCache;
use crate::state::{AppState, PendingFiles};

/// Extract file paths from a process argument list, skipping the executable and
/// any flags, and keeping only arguments that point at an existing file. This is
/// how "Open with CEESVEE" hands us the file on Windows and Linux.
fn files_from_args(args: impl IntoIterator<Item = String>) -> Vec<String> {
    args.into_iter()
        .skip(1)
        .filter(|arg| !arg.starts_with('-'))
        .filter(|arg| std::path::Path::new(arg).is_file())
        .collect()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let initial_files = files_from_args(std::env::args());

    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default();

    // Single-instance must be registered first so it can intercept a second
    // launch (e.g. opening another CSV) before a new window is created, and
    // forward the file to the window that's already open.
    #[cfg(desktop)]
    {
        use tauri::{Emitter, Manager};
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            let files = files_from_args(argv);
            if !files.is_empty() {
                let _ = app.emit("open-files", files);
            }
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }));
    }

    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(Mutex::new(AppState::default()))
        .manage(PendingFiles(Mutex::new(initial_files)))
        .manage(JobRegistry::default())
        .manage(DiagnosticsCache::default())
        .manage(ProfileCache::default())
        .manage(crate::schema_ops::SchemaScanCache::default())
        .manage(DedupCache::default())
        .manage(CompareCache::default())
        .manage(crate::archive::ArchiveCache::default())
        .manage(crate::cluster::ClusterCache::default())
        .manage(crate::semantic::SemanticCache::default())
        .manage(crate::crossval::CrossValCache::default())
        .manage(crate::outlier::OutlierCache::default())
        .manage(crate::append::AppendCache::default())
        .manage(crate::recipe::RecipeCache::default())
        .manage(crate::pii::PiiCache::default())
        .manage(crate::follow::FollowRegistry::default())
        .manage(crate::json_import::JsonImportPreviewCache::default())
        .manage(crate::project::ProjectStore::default())
        .manage(crate::annotations::AnnotationRegistry::default())
        .setup(|app| {
            // Delete index caches orphaned by an abnormal termination. Live
            // instances hold their cache's lock file, so they are skipped.
            use tauri::Manager;
            if let Ok(cache_dir) = app.path().app_cache_dir() {
                let root = cache_dir.join("indexes");
                std::thread::spawn(move || index::sweep_stale(&root));
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::open_file,
            commands::list_archive_entries,
            commands::start_archive_extract,
            commands::pending_archive_estimate,
            commands::open_archive_document,
            commands::discard_archive,
            commands::probe_open,
            commands::start_open_indexed,
            commands::start_convert_to_editable,
            commands::start_reindex,
            commands::preview_reparse,
            commands::apply_reparse,
            commands::preview_dialect,
            commands::apply_dialect,
            commands::start_follow,
            commands::set_follow_paused,
            commands::stop_follow,
            commands::set_row_range_filter,
            commands::get_file_fingerprint,
            commands::check_external_change,
            commands::new_document,
            commands::close_document,
            commands::get_meta,
            commands::list_encodings,
            commands::take_pending_files,
            commands::cancel_job,
            commands::get_diagnostics,
            commands::start_diagnostics_scan,
            commands::apply_diagnostic_filter,
            commands::get_rows,
            commands::get_cell,
            commands::selection_stats,
            commands::column_summaries,
            commands::set_cell,
            commands::set_cells,
            commands::paste,
            commands::copy_as,
            commands::preview_paste_special,
            commands::apply_paste_special,
            commands::insert_rows,
            commands::delete_rows,
            commands::move_row,
            commands::insert_column,
            commands::delete_columns,
            commands::rename_column,
            commands::move_column,
            commands::sort,
            commands::set_header_mode,
            commands::set_filter,
            commands::clear_filter,
            commands::set_view_sort,
            commands::reset_row_view,
            commands::find,
            commands::replace_all,
            commands::undo,
            commands::redo,
            commands::get_schema,
            commands::start_infer_schema,
            commands::take_inferred_schema,
            commands::set_column_schema,
            commands::remove_column_schema,
            commands::export_schema,
            commands::import_schema,
            commands::validate_cell_edit,
            commands::get_schema_issues,
            commands::clear_schema_issues,
            commands::start_schema_invalid_samples,
            commands::take_schema_invalid_samples,
            commands::start_convert_column_preview,
            commands::take_convert_column_preview,
            commands::convert_column_apply,
            commands::check_encoding_compatibility,
            commands::export_scope_counts,
            commands::start_save,
            commands::start_export,
            commands::json_import_preview,
            commands::get_json_import_preview,
            commands::json_import_apply,
            commands::json_export,
            commands::get_settings,
            commands::set_settings,
            commands::validate_profile,
            commands::get_column_profile,
            commands::start_column_profile,
            commands::preview_transform,
            commands::apply_transform,
            commands::get_duplicate_report,
            commands::start_duplicate_scan,
            commands::apply_duplicate_filter,
            commands::apply_deduplicate,
            commands::get_cluster_report,
            commands::start_cluster_scan,
            commands::apply_value_clusters,
            commands::get_semantic_report,
            commands::start_semantic_scan,
            commands::apply_semantic_filter,
            commands::preview_semantic_action,
            commands::apply_semantic_action,
            commands::get_crossval_report,
            commands::start_crossval_scan,
            commands::apply_crossval_filter,
            commands::preview_repair,
            commands::apply_repair,
            commands::get_outlier_report,
            commands::start_outlier_scan,
            commands::apply_outlier_filter,
            commands::preview_outlier_action,
            commands::apply_outlier_action,
            commands::preview_append,
            commands::start_append,
            commands::get_append_report,
            commands::preview_join,
            commands::start_join,
            commands::preview_group_by,
            commands::start_group_by,
            commands::preview_reshape,
            commands::start_reshape,
            commands::validate_recipe_batch,
            commands::start_recipe_batch,
            commands::get_batch_report,
            commands::get_pii_report,
            commands::start_pii_scan,
            commands::preview_redaction,
            commands::apply_redaction,
            commands::get_dictionary,
            commands::set_dictionary_field,
            commands::remove_dictionary_field,
            commands::discard_dictionary_orphans,
            commands::export_dictionary,
            commands::preview_dictionary_import,
            commands::apply_dictionary_import,
            commands::get_changes,
            commands::revert_change,
            commands::revert_change_cells,
            commands::revert_column_changes,
            commands::revert_all_changes,
            commands::list_recovery_sessions,
            commands::recover_session,
            commands::discard_recovery_session,
            commands::delete_all_recovery,
            commands::start_compare,
            commands::get_compare_info,
            commands::get_compare_results,
            commands::start_compare_export,
            project::project_new,
            project::project_get,
            project::project_set_section,
            project::project_save,
            project::project_save_as,
            project::project_save_template,
            project::project_close,
            project::project_open_preview,
            project::project_open_apply,
            commands::preview_sample,
            commands::start_sample,
            commands::annotations_view,
            commands::annotations_rematch,
            commands::annotations_set_key_spec,
            commands::annotations_set_author,
            commands::annotations_edit_row,
            commands::annotations_set_row_note,
            commands::annotations_set_cell_note,
            commands::annotations_remove_row,
            commands::annotations_discard_orphans,
            commands::annotations_define_tag,
            commands::annotations_remove_tag,
            commands::apply_annotation_filter,
            commands::preview_tag_to_column,
            commands::apply_tag_to_column,
            commands::export_annotations,
            commands::annotations_get_export,
            commands::annotations_load_export,
            commands::annotations_load_sidecar,
            commands::annotations_save_sidecar,
        ])
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(|_app, _event| {
            // macOS delivers "Open with" via an application event, not argv.
            #[cfg(target_os = "macos")]
            {
                use tauri::{Emitter, Manager};
                if let tauri::RunEvent::Opened { urls } = _event {
                    let files: Vec<String> = urls
                        .iter()
                        .filter_map(|u| u.to_file_path().ok())
                        .map(|p| p.to_string_lossy().to_string())
                        .collect();
                    if !files.is_empty() {
                        if let Some(state) = _app.try_state::<PendingFiles>() {
                            if let Ok(mut pending) = state.0.lock() {
                                pending.extend(files.clone());
                            }
                        }
                        let _ = _app.emit("open-files", files);
                    }
                }
            }
        });
}
