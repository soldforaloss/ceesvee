//! Process-wide application state: the set of open documents, keyed by id.
//! Wrapped in a `Mutex` and handed to Tauri via `Builder::manage`.

use std::collections::HashMap;

use crate::document::Document;
use crate::error::{AppError, AppResult};

#[derive(Default)]
pub struct AppState {
    docs: HashMap<u64, Document>,
    next_id: u64,
}

impl AppState {
    /// Allocate a fresh, monotonically increasing document id.
    pub fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn insert(&mut self, doc: Document) {
        self.docs.insert(doc.id, doc);
    }

    pub fn get(&self, id: u64) -> AppResult<&Document> {
        self.docs.get(&id).ok_or(AppError::DocNotFound(id))
    }

    pub fn get_mut(&mut self, id: u64) -> AppResult<&mut Document> {
        self.docs.get_mut(&id).ok_or(AppError::DocNotFound(id))
    }

    pub fn remove(&mut self, id: u64) -> bool {
        self.docs.remove(&id).is_some()
    }
}
