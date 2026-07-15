//! Process-wide application state: the set of open documents, keyed by id.
//!
//! The registry itself lives behind a `Mutex` handed to Tauri via
//! `Builder::manage`, but each document is wrapped in its own
//! `Arc<RwLock<Document>>`. Commands lock the registry only long enough to
//! clone the document's `Arc`, then release it — so a long scan or export
//! holding one document's lock never blocks work on other open tabs, and
//! never blocks opening or closing documents.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use crate::document::Document;
use crate::error::{AppError, AppResult};

/// A single open document behind its own reader/writer lock.
pub type SharedDocument = Arc<RwLock<Document>>;

/// Files passed on the command line at launch (e.g. "Open with CEESVEE"),
/// waiting to be drained by the front end once it mounts.
#[derive(Default)]
pub struct PendingFiles(pub Mutex<Vec<String>>);

#[derive(Default)]
pub struct AppState {
    docs: HashMap<u64, SharedDocument>,
    next_id: u64,
}

impl AppState {
    /// Allocate a fresh, monotonically increasing document id.
    pub fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn insert(&mut self, doc: Document) {
        self.docs.insert(doc.id, Arc::new(RwLock::new(doc)));
    }

    /// Clone the handle for a document so its lock can be taken after the
    /// registry lock has been released.
    pub fn doc(&self, id: u64) -> AppResult<SharedDocument> {
        self.docs.get(&id).cloned().ok_or(AppError::DocNotFound(id))
    }

    pub fn remove(&mut self, id: u64) -> bool {
        self.docs.remove(&id).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_are_independently_lockable() {
        let mut state = AppState::default();
        let a_id = state.alloc_id();
        state.insert(Document::new_empty(a_id, 2, 2));
        let b_id = state.alloc_id();
        state.insert(Document::new_empty(b_id, 2, 2));

        // Holding a write lock on one document must not prevent reading the
        // other (the per-tab independence the job system relies on).
        let a = state.doc(a_id).unwrap();
        let b = state.doc(b_id).unwrap();
        let _a_write = a.write().unwrap();
        let b_read = b.try_read();
        assert!(b_read.is_ok(), "other documents stay readable");
    }

    #[test]
    fn missing_document_errors() {
        let state = AppState::default();
        assert!(matches!(state.doc(99), Err(AppError::DocNotFound(99))));
    }

    #[test]
    fn remove_forgets_document() {
        let mut state = AppState::default();
        let id = state.alloc_id();
        state.insert(Document::new_empty(id, 1, 1));
        assert!(state.remove(id));
        assert!(!state.remove(id));
        assert!(state.doc(id).is_err());
    }
}
