//! CEESVEE core library: a Rust-owned, in-memory CSV/delimited-file model
//! exposed to the web front end through a small Tauri command surface.

mod analyze;
mod commands;
mod delimiter;
mod diagnostics;
mod document;
mod dto;
mod encoding;
mod error;
mod export;
mod filter;
mod find;
/// Public so downstream features (and the test harness) can treat the job
/// registry, progress plumbing and cancellation as a stable internal API.
pub mod job;
mod parse;
mod reopen;
mod sort;
mod state;
mod util;

use std::sync::Mutex;

use crate::diagnostics::DiagnosticsCache;
use crate::job::JobRegistry;
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
        .invoke_handler(tauri::generate_handler![
            commands::open_file,
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
            commands::selection_stats,
            commands::column_summaries,
            commands::set_cell,
            commands::set_cells,
            commands::paste,
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
            commands::save,
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
