//! Crash recovery and edit journaling (F16). OPT-IN: when enabled, every
//! editable document with a source file keeps an append-only journal of its
//! edit operations (JSON lines: a header, then one record per operation,
//! including undo/redo markers). After a crash the journal replays those
//! operations onto a fresh parse of the source — the source file itself is
//! NEVER written during recovery, and a changed source fingerprint blocks
//! blind replay. Journals reset on every successful save (the compaction
//! step, via atomic temp+rename), are deleted on clean close, and corrupt
//! trailing data never invalidates the complete records before it.
//! PRIVACY: journals contain edited cell values; the UI shows a disclosure,
//! retention is configurable, and "Delete all recovery data" wipes the
//! directory.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::dto::FileFingerprint;
use crate::error::{AppError, AppResult};

/// Journal format version; incompatible files are kept for manual recovery.
pub const JOURNAL_VERSION: u32 = 1;

/// First line of every journal.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalHeader {
    pub version: u32,
    /// Source file the journal's base state comes from.
    pub path: String,
    /// Fingerprint of the source at journaling time.
    pub fingerprint: Option<FileFingerprint>,
    /// Parse interpretation the base state used.
    pub delimiter: String,
    pub encoding: String,
    pub has_header_row: bool,
    /// Revision at the journal's base state (diagnostic only).
    pub base_revision: u64,
}

/// One journal line after the header. `Op` carries a serialized `EditOp`
/// (document.rs owns that type; it round-trips through serde_json).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum JournalRecord {
    Op { op: serde_json::Value },
    Undo,
    Redo,
}

/// Append-only writer for one document's journal. Every record is flushed
/// immediately (edits are user-paced; durability beats batching here).
pub struct JournalWriter {
    path: PathBuf,
    file: std::fs::File,
}

impl JournalWriter {
    /// Create a fresh journal (truncating any previous file at this path).
    pub fn create(path: PathBuf, header: &JournalHeader) -> AppResult<JournalWriter> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(&path)?;
        let line = serde_json::to_string(header)
            .map_err(|e| AppError::Other(format!("journal header: {e}")))?;
        writeln!(file, "{line}")?;
        file.sync_data()?;
        Ok(JournalWriter { path, file })
    }

    pub fn append(&mut self, record: &JournalRecord) {
        // Best-effort: a failing journal write must never block editing.
        if let Ok(line) = serde_json::to_string(record) {
            let _ = writeln!(self.file, "{line}");
            let _ = self.file.flush();
        }
    }

    /// Reset to a fresh header after a successful save — the journal's
    /// compaction step. Atomic: written beside, then renamed over.
    pub fn reset(&mut self, header: &JournalHeader) -> AppResult<()> {
        let tmp = self.path.with_extension("journal-tmp");
        {
            let mut file = std::fs::File::create(&tmp)?;
            let line = serde_json::to_string(header)
                .map_err(|e| AppError::Other(format!("journal header: {e}")))?;
            writeln!(file, "{line}")?;
            file.sync_data()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        self.file = std::fs::OpenOptions::new().append(true).open(&self.path)?;
        Ok(())
    }

    /// Delete the journal (clean close or successful recovery hand-off).
    pub fn delete(self) {
        drop(self.file);
        let _ = std::fs::remove_file(&self.path);
    }
}

/// One recoverable session found at startup.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoverableSession {
    /// Journal file (absolute), the handle for recover/discard.
    pub journal_path: String,
    pub source_path: String,
    pub file_name: String,
    /// Seconds since the epoch of the journal's last modification.
    pub last_edit_epoch_secs: u64,
    pub operation_count: usize,
    /// The source changed (or vanished) since journaling — blind replay is
    /// blocked and Open Copy becomes the default.
    pub source_changed: bool,
    pub source_missing: bool,
    /// Journal version mismatch: kept on disk for manual recovery only.
    pub incompatible: bool,
}

/// Parse a journal: header + records, tolerating corrupt trailing data
/// (parsing stops at the first bad line; everything before it survives).
pub fn read_journal(path: &Path) -> AppResult<(JournalHeader, Vec<JournalRecord>)> {
    let file = std::fs::File::open(path)?;
    let mut lines = std::io::BufReader::new(file).lines();
    let header_line = lines
        .next()
        .ok_or_else(|| AppError::invalid("empty journal"))??;
    let header: JournalHeader = serde_json::from_str(&header_line)
        .map_err(|_| AppError::invalid("unreadable journal header"))?;
    if header.version != JOURNAL_VERSION {
        return Err(AppError::invalid(format!(
            "journal version {} is not supported (this build understands {JOURNAL_VERSION})",
            header.version
        )));
    }
    let mut records = Vec::new();
    for line in lines {
        let Ok(line) = line else { break };
        match serde_json::from_str::<JournalRecord>(&line) {
            Ok(record) => records.push(record),
            Err(_) => break, // corrupt tail: keep the complete prefix
        }
    }
    Ok((header, records))
}

/// Scan the recovery directory for sessions to offer at startup.
pub fn scan_recoverable(dir: &Path) -> Vec<RecoverableSession> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("journal") {
            continue;
        }
        let modified = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        match read_journal(&path) {
            Ok((header, records)) => {
                // A journal with no operations has nothing to recover.
                if records.is_empty() {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                let source = PathBuf::from(&header.path);
                let disk = crate::util::stat_fingerprint(&source);
                let source_missing = !source.is_file();
                let source_changed = match (&header.fingerprint, &disk) {
                    (Some(a), Some(b)) => a != b,
                    _ => true,
                };
                out.push(RecoverableSession {
                    journal_path: path.display().to_string(),
                    source_path: header.path.clone(),
                    file_name: source
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| header.path.clone()),
                    last_edit_epoch_secs: modified,
                    operation_count: records
                        .iter()
                        .filter(|r| matches!(r, JournalRecord::Op { .. }))
                        .count(),
                    source_changed: source_changed && !source_missing,
                    source_missing,
                    incompatible: false,
                });
            }
            Err(e) if e.to_string().contains("version") => {
                out.push(RecoverableSession {
                    journal_path: path.display().to_string(),
                    source_path: String::new(),
                    file_name: path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                    last_edit_epoch_secs: modified,
                    operation_count: 0,
                    source_changed: false,
                    source_missing: false,
                    incompatible: true,
                });
            }
            Err(_) => {
                // Unreadable header: keep the file (manual recovery), but
                // don't offer it.
            }
        }
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.last_edit_epoch_secs));
    out
}

/// Delete journals older than the retention window; returns how many went.
pub fn sweep_expired(dir: &Path, retention_days: u32) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let cutoff = std::time::SystemTime::now()
        - std::time::Duration::from_secs(u64::from(retention_days) * 24 * 3600);
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("journal") {
            continue;
        }
        let old = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|t| t < cutoff)
            .unwrap_or(false);
        if old && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Delete every journal ("Delete all recovery data").
pub fn delete_all(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str());
        if matches!(ext, Some("journal") | Some("journal-tmp"))
            && std::fs::remove_file(&path).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(path: &str) -> JournalHeader {
        JournalHeader {
            version: JOURNAL_VERSION,
            path: path.to_string(),
            fingerprint: None,
            delimiter: ",".into(),
            encoding: "UTF-8".into(),
            has_header_row: true,
            base_revision: 0,
        }
    }

    #[test]
    fn journal_round_trips_and_tolerates_corrupt_tails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.journal");
        let mut writer = JournalWriter::create(path.clone(), &header("x.csv")).unwrap();
        writer.append(&JournalRecord::Op {
            op: serde_json::json!({"k": 1}),
        });
        writer.append(&JournalRecord::Undo);
        writer.append(&JournalRecord::Redo);
        drop(writer);

        // Corrupt trailing data (a torn write) is ignored.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            write!(f, "{{\"type\":\"op\",\"op\":{{tr").unwrap();
        }
        let (h, records) = read_journal(&path).unwrap();
        assert_eq!(h.path, "x.csv");
        assert_eq!(records.len(), 3, "the complete prefix survives");
        assert!(matches!(records[1], JournalRecord::Undo));
    }

    #[test]
    fn reset_compacts_atomically_and_delete_removes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.journal");
        let mut writer = JournalWriter::create(path.clone(), &header("x.csv")).unwrap();
        writer.append(&JournalRecord::Undo);
        writer.reset(&header("x.csv")).unwrap();
        let (_, records) = read_journal(&path).unwrap();
        assert!(records.is_empty(), "reset leaves only the header");
        writer.append(&JournalRecord::Undo);
        let (_, records) = read_journal(&path).unwrap();
        assert_eq!(records.len(), 1, "appending continues after a reset");
        writer.delete();
        assert!(!path.exists());
    }

    #[test]
    fn scan_reports_sessions_and_version_mismatches() {
        let dir = tempfile::tempdir().unwrap();
        // A good journal with one op (missing source -> flagged).
        let good = dir.path().join("good.journal");
        let mut w = JournalWriter::create(good, &header("Z:/nope/gone.csv")).unwrap();
        w.append(&JournalRecord::Op {
            op: serde_json::json!({}),
        });
        drop(w);
        // An op-less journal is cleaned up silently.
        let empty = dir.path().join("empty.journal");
        drop(JournalWriter::create(empty.clone(), &header("y.csv")).unwrap());
        // A future-version journal is kept and flagged incompatible.
        let future = dir.path().join("future.journal");
        std::fs::write(
            &future,
            "{\"version\":99,\"path\":\"p\",\"fingerprint\":null,\"delimiter\":\",\",\
             \"encoding\":\"UTF-8\",\"hasHeaderRow\":true,\"baseRevision\":0}\n",
        )
        .unwrap();

        let sessions = scan_recoverable(dir.path());
        assert_eq!(sessions.len(), 2);
        assert!(!empty.exists(), "empty journals are swept");
        let good_session = sessions.iter().find(|s| !s.incompatible).unwrap();
        assert!(good_session.source_missing);
        assert_eq!(good_session.operation_count, 1);
        assert!(sessions.iter().any(|s| s.incompatible));
        assert!(
            future.exists(),
            "incompatible journals stay for manual recovery"
        );
    }

    #[test]
    fn delete_all_wipes_journals() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = JournalWriter::create(dir.path().join("a.journal"), &header("x.csv")).unwrap();
        w.append(&JournalRecord::Undo);
        drop(w);
        std::fs::write(dir.path().join("keep.txt"), "not a journal").unwrap();
        assert_eq!(delete_all(dir.path()), 1);
        assert!(dir.path().join("keep.txt").exists());
    }
}
