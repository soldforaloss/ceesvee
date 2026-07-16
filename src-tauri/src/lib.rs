//! CEESVEE core library: a Rust-owned, in-memory CSV/delimited-file model
//! exposed to the web front end through a small Tauri command surface.

mod analyze;
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
mod document;
mod dto;
mod encoding;
mod error;
mod export;
mod export_scope;
mod filter;
mod find;
mod groupby;
mod index;
/// Public so downstream features (and the test harness) can treat the job
/// registry, progress plumbing and cancellation as a stable internal API.
pub mod job;
mod joins;
mod outlier;
mod parse;
mod paste;
mod pii;
mod profile;
mod recipe;
mod reopen;
mod repair;
mod reshape;
mod save;
mod semantic;
mod settings;
mod sort;
mod state;
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
            commands::find,
            commands::replace_all,
            commands::undo,
            commands::redo,
            commands::check_encoding_compatibility,
            commands::export_scope_counts,
            commands::start_save,
            commands::start_export,
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
            commands::get_changes,
            commands::revert_change,
            commands::revert_change_cells,
            commands::revert_column_changes,
            commands::revert_all_changes,
            commands::start_compare,
            commands::get_compare_info,
            commands::get_compare_results,
            commands::start_compare_export,
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
