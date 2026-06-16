//! CEESVEE core library: a Rust-owned, in-memory CSV/delimited-file model
//! exposed to the web front end through a small Tauri command surface.

mod commands;
mod delimiter;
mod document;
mod dto;
mod encoding;
mod error;
mod export;
mod find;
mod parse;
mod sort;
mod state;
mod util;

use std::sync::Mutex;

use crate::state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .manage(Mutex::new(AppState::default()))
        .invoke_handler(tauri::generate_handler![
            commands::open_file,
            commands::reparse,
            commands::new_document,
            commands::close_document,
            commands::get_meta,
            commands::list_encodings,
            commands::get_rows,
            commands::selection_stats,
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
            commands::find,
            commands::replace_all,
            commands::undo,
            commands::redo,
            commands::save,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
