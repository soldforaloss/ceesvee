//! Follow/tail mode (F19): watch a growing CSV inside CEESVEE. Follow
//! documents are READ-ONLY; a per-document watcher thread polls the file
//! for appended bytes, feeds them through an incremental quote-aware
//! record splitter (a partial trailing record — including an open quoted
//! field — stays hidden until it completes), and appends complete rows to
//! the document. Truncation, replacement, and rotation are detected (the
//! file shrank or its identity changed) and reported instead of silently
//! mixing old and new content; watchers stop and release their handles
//! when the tab closes.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::error::AppResult;

/// Poll interval for appended bytes.
const POLL_MS: u64 = 500;
/// Appended bytes are read in chunks of this size.
const READ_CHUNK: usize = 256 * 1024;
/// Event channel for appended rows.
pub const FOLLOW_UPDATE_EVENT: &str = "follow-update";
/// Event channel for truncation/rotation/width/encoding alerts.
pub const FOLLOW_ALERT_EVENT: &str = "follow-alert";

/// Payload for `follow-update`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FollowUpdate {
    pub doc_id: u64,
    pub new_rows: usize,
    pub total_rows: usize,
    pub revision: u64,
}

/// Why the watcher raised an alert (and paused itself).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FollowAlertKind {
    /// The file shrank or was replaced — old and new content are never
    /// combined silently.
    TruncatedOrRotated,
    /// An appended record is wider than the document.
    WidthChanged,
    /// Appended bytes are not valid UTF-8 under the opened encoding.
    EncodingChanged,
    /// The file disappeared.
    Missing,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FollowAlert {
    pub doc_id: u64,
    pub kind: FollowAlertKind,
}

/// Incremental, quote-aware record splitter (RFC 4180 subset: the given
/// delimiter, `"` quoting with doubling). Bytes may arrive in arbitrary
/// chunks; an incomplete trailing record — including an open quoted field —
/// is carried until it completes.
#[derive(Default)]
pub struct IncrementalCsv {
    carry: Vec<u8>,
    in_quotes: bool,
}

impl IncrementalCsv {
    pub fn new() -> IncrementalCsv {
        IncrementalCsv::default()
    }

    /// How many carried bytes are waiting for their record to complete.
    #[cfg(test)]
    pub fn pending_bytes(&self) -> usize {
        self.carry.len()
    }

    /// Feed appended bytes; returns the COMPLETE record LINES they finished
    /// (raw bytes, so the caller can validate the encoding per record —
    /// record boundaries are newlines, which never split a multibyte
    /// character — before splitting fields).
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut lines = Vec::new();
        for &b in bytes {
            if b == b'"' {
                // Toggling on every quote handles doubling too: "" flips
                // out and straight back in, leaving the state correct.
                self.in_quotes = !self.in_quotes;
            }
            if b == b'\n' && !self.in_quotes {
                // A record terminator OUTSIDE quotes completes the carry.
                let mut line = std::mem::take(&mut self.carry);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if !line.is_empty() {
                    lines.push(line);
                }
                continue;
            }
            self.carry.push(b);
        }
        lines
    }
}

/// Split one complete record line into fields (quote-aware, "" unescapes).
fn split_record(line: &[u8], delimiter: u8) -> Vec<String> {
    let mut fields: Vec<String> = Vec::new();
    let mut field: Vec<u8> = Vec::new();
    let mut in_quotes = false;
    let mut i = 0;
    while i < line.len() {
        let b = line[i];
        if in_quotes {
            if b == b'"' {
                if line.get(i + 1) == Some(&b'"') {
                    field.push(b'"');
                    i += 2;
                    continue;
                }
                in_quotes = false;
            } else {
                field.push(b);
            }
        } else if b == b'"' {
            in_quotes = true;
        } else if b == delimiter {
            fields.push(String::from_utf8_lossy(&field).into_owned());
            field.clear();
        } else {
            field.push(b);
        }
        i += 1;
    }
    fields.push(String::from_utf8_lossy(&field).into_owned());
    fields
}

/// Control block for one running watcher.
pub struct FollowControl {
    pub paused: Arc<AtomicBool>,
    pub stop: Arc<AtomicBool>,
}

/// Watchers per followed document, managed by Tauri.
#[derive(Default)]
pub struct FollowRegistry(Mutex<HashMap<u64, FollowControl>>);

impl FollowRegistry {
    pub fn insert(&self, doc_id: u64, control: FollowControl) {
        if let Ok(mut map) = self.0.lock() {
            map.insert(doc_id, control);
        }
    }

    pub fn set_paused(&self, doc_id: u64, paused: bool) -> bool {
        self.0
            .lock()
            .ok()
            .and_then(|map| {
                map.get(&doc_id)
                    .map(|c| c.paused.store(paused, Ordering::Relaxed))
            })
            .is_some()
    }

    /// Signal the watcher to stop and forget it. Idempotent.
    pub fn stop(&self, doc_id: u64) {
        if let Ok(mut map) = self.0.lock() {
            if let Some(control) = map.remove(&doc_id) {
                control.stop.store(true, Ordering::Relaxed);
            }
        }
    }
}

/// Identity of the followed file, captured when the watch starts. A rotation
/// that replaces the file can leave the new size EQUAL OR LARGER than the
/// old offset, so size alone cannot detect it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileIdentity {
    /// Unix: the inode number — changes whenever the path points at a new
    /// file.
    #[cfg(unix)]
    ino: u64,
    /// Windows has no inode in std; the creation time changes when the path
    /// is replaced (best-effort: some filesystems don't report it, and NTFS
    /// "tunneling" can briefly preserve it — the size-shrink check still
    /// backstops those cases).
    created: Option<std::time::SystemTime>,
}

impl FileIdentity {
    pub fn of(metadata: &std::fs::Metadata) -> FileIdentity {
        FileIdentity {
            #[cfg(unix)]
            ino: std::os::unix::fs::MetadataExt::ino(metadata),
            created: metadata.created().ok(),
        }
    }
}

/// Everything the watcher thread needs (no Tauri types, so it is testable).
pub struct WatcherConfig {
    pub doc_id: u64,
    pub path: PathBuf,
    /// Byte offset the initial full parse consumed up to (the file length
    /// at open).
    pub start_offset: u64,
    pub delimiter: u8,
    pub n_cols: usize,
    /// Identity at open; a mismatch on any poll means the file was replaced.
    pub identity: FileIdentity,
    /// Validate appended records as UTF-8 (the document's opened encoding).
    pub require_utf8: bool,
}

/// One poll step: check the file and drain any complete appended records.
/// Returns rows to append, or an alert. Pure with respect to documents —
/// the caller owns applying rows and emitting events.
pub fn poll_step(
    config: &WatcherConfig,
    offset: &mut u64,
    parser: &mut IncrementalCsv,
) -> AppResult<Result<Vec<Vec<String>>, FollowAlertKind>> {
    let metadata = match std::fs::metadata(&config.path) {
        Ok(m) => m,
        Err(_) => return Ok(Err(FollowAlertKind::Missing)),
    };
    if FileIdentity::of(&metadata) != config.identity {
        // The path points at a DIFFERENT file now (rotation/replacement) —
        // even if its size grew past our offset. Never silently mix content.
        return Ok(Err(FollowAlertKind::TruncatedOrRotated));
    }
    let len = metadata.len();
    if len < *offset {
        // The file shrank: truncation, replacement, or rotation. Never
        // silently combine old and new content.
        return Ok(Err(FollowAlertKind::TruncatedOrRotated));
    }
    if len == *offset {
        return Ok(Ok(Vec::new()));
    }

    let mut file = std::fs::File::open(&config.path)?;
    file.seek(SeekFrom::Start(*offset))?;
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut buf = vec![0u8; READ_CHUNK];
    let mut remaining = len - *offset;
    while remaining > 0 {
        let want = remaining.min(READ_CHUNK as u64) as usize;
        let n = file.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        for line in parser.feed(&buf[..n]) {
            // Validate per COMPLETE record (newline boundaries never split
            // a multibyte character, so chunk edges can't false-positive):
            // any invalid sequence means the producer changed encodings.
            if config.require_utf8 && std::str::from_utf8(&line).is_err() {
                return Ok(Err(FollowAlertKind::EncodingChanged));
            }
            // Catch-all for non-UTF-8-followed files: NUL bytes mean the
            // appended data is not delimited text any more.
            if line.contains(&0) {
                return Ok(Err(FollowAlertKind::EncodingChanged));
            }
            let record = split_record(&line, config.delimiter);
            if record.len() > config.n_cols {
                return Ok(Err(FollowAlertKind::WidthChanged));
            }
            rows.push(record);
        }
        *offset += n as u64;
        remaining -= n as u64;
    }
    Ok(Ok(rows))
}

/// Spawn the watcher thread for one followed document.
#[allow(clippy::too_many_arguments)]
pub fn spawn_watcher(
    app: tauri::AppHandle,
    registry_handle: crate::state::SharedDocument,
    config: WatcherConfig,
) -> FollowControl {
    use tauri::Emitter;
    let paused = Arc::new(AtomicBool::new(false));
    let stop = Arc::new(AtomicBool::new(false));
    let control = FollowControl {
        paused: Arc::clone(&paused),
        stop: Arc::clone(&stop),
    };

    std::thread::spawn(move || {
        let mut offset = config.start_offset;
        let mut parser = IncrementalCsv::new();
        let mut alerted = false;
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(POLL_MS));
            if paused.load(Ordering::Relaxed) || alerted {
                // Paused: view updates stop, but the FILE keeps its bytes —
                // nothing is lost; polling resumes from the same offset.
                continue;
            }
            match poll_step(&config, &mut offset, &mut parser) {
                Ok(Ok(rows)) if rows.is_empty() => {}
                Ok(Ok(rows)) => {
                    let update = {
                        let Ok(mut doc) = registry_handle.write() else {
                            break;
                        };
                        let new_rows = rows.len();
                        doc.append_follow_rows(rows);
                        FollowUpdate {
                            doc_id: config.doc_id,
                            new_rows,
                            total_rows: doc.n_rows(),
                            revision: doc.revision(),
                        }
                    };
                    let _ = app.emit(FOLLOW_UPDATE_EVENT, &update);
                }
                Ok(Err(kind)) => {
                    alerted = true;
                    let _ = app.emit(
                        FOLLOW_ALERT_EVENT,
                        &FollowAlert {
                            doc_id: config.doc_id,
                            kind,
                        },
                    );
                }
                Err(_) => {
                    alerted = true;
                    let _ = app.emit(
                        FOLLOW_ALERT_EVENT,
                        &FollowAlert {
                            doc_id: config.doc_id,
                            kind: FollowAlertKind::Missing,
                        },
                    );
                }
            }
        }
    });

    control
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test shorthand: feed bytes and split the finished lines into fields.
    fn feed_records(p: &mut IncrementalCsv, bytes: &[u8]) -> Vec<Vec<String>> {
        p.feed(bytes)
            .into_iter()
            .map(|line| split_record(&line, b','))
            .collect()
    }

    fn identity_of(path: &std::path::Path) -> FileIdentity {
        FileIdentity::of(&std::fs::metadata(path).unwrap())
    }

    #[test]
    fn incremental_parser_holds_partial_records_until_complete() {
        let mut p = IncrementalCsv::new();
        // A record arriving in three fragments.
        assert!(p.feed(b"1,al").is_empty());
        assert!(p.feed(b"pha").is_empty());
        let records = feed_records(&mut p, b"\n2,beta\n3,");
        assert_eq!(records, vec![vec!["1", "alpha"], vec!["2", "beta"]]);
        assert!(p.pending_bytes() > 0, "the partial third record is carried");
        let records = feed_records(&mut p, b"gamma\n");
        assert_eq!(records, vec![vec!["3", "gamma"]]);
    }

    #[test]
    fn open_quoted_fields_stay_hidden_until_closed() {
        let mut p = IncrementalCsv::new();
        // A newline INSIDE an open quote does not complete the record.
        assert!(p.feed(b"1,\"multi\nline").is_empty());
        let records = feed_records(&mut p, b" still\"\n");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0][1], "multi\nline still");
    }

    #[test]
    fn quotes_unescape_and_crlf_is_handled() {
        let mut p = IncrementalCsv::new();
        let records = feed_records(&mut p, b"a,\"he said \"\"hi\"\"\"\r\nb,2\r\n");
        assert_eq!(records[0][1], "he said \"hi\"");
        assert_eq!(records[1], vec!["b", "2"]);
    }

    #[test]
    fn poll_step_reads_only_appended_bytes_and_detects_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.csv");
        std::fs::write(&path, "id,msg\n1,start\n").unwrap();
        let start = std::fs::metadata(&path).unwrap().len();
        let config = WatcherConfig {
            doc_id: 1,
            path: path.clone(),
            start_offset: start,
            delimiter: b',',
            n_cols: 2,
            identity: identity_of(&path),
            require_utf8: true,
        };
        let mut offset = start;
        let mut parser = IncrementalCsv::new();

        // Nothing appended yet.
        assert!(poll_step(&config, &mut offset, &mut parser)
            .unwrap()
            .unwrap()
            .is_empty());

        // Append one complete and one partial record.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            write!(f, "2,appended\n3,par").unwrap();
        }
        let rows = poll_step(&config, &mut offset, &mut parser)
            .unwrap()
            .unwrap();
        assert_eq!(rows, vec![vec!["2", "appended"]]);

        // Completing the record surfaces it.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, "tial").unwrap();
        }
        let rows = poll_step(&config, &mut offset, &mut parser)
            .unwrap()
            .unwrap();
        assert_eq!(rows, vec![vec!["3", "partial"]]);

        // Truncation is an alert, never a silent merge.
        std::fs::write(&path, "fresh\n").unwrap();
        let alert = poll_step(&config, &mut offset, &mut parser).unwrap();
        assert_eq!(alert.unwrap_err(), FollowAlertKind::TruncatedOrRotated);
    }

    #[test]
    fn wider_records_and_missing_files_raise_alerts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.csv");
        std::fs::write(&path, "a,b\n").unwrap();
        let start = std::fs::metadata(&path).unwrap().len();
        let config = WatcherConfig {
            doc_id: 1,
            path: path.clone(),
            start_offset: start,
            delimiter: b',',
            n_cols: 2,
            identity: identity_of(&path),
            require_utf8: true,
        };
        let mut offset = start;
        let mut parser = IncrementalCsv::new();
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, "1,2,3").unwrap();
        }
        let alert = poll_step(&config, &mut offset, &mut parser).unwrap();
        assert_eq!(alert.unwrap_err(), FollowAlertKind::WidthChanged);

        std::fs::remove_file(&path).unwrap();
        let mut offset2 = 0;
        let alert = poll_step(&config, &mut offset2, &mut parser).unwrap();
        assert_eq!(alert.unwrap_err(), FollowAlertKind::Missing);
    }

    #[test]
    fn invalid_utf8_records_raise_encoding_alerts_without_nul() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.csv");
        std::fs::write(&path, "a,b\n").unwrap();
        let start = std::fs::metadata(&path).unwrap().len();
        let config = WatcherConfig {
            doc_id: 1,
            path: path.clone(),
            start_offset: start,
            delimiter: b',',
            n_cols: 2,
            identity: identity_of(&path),
            require_utf8: true,
        };
        let mut offset = start;
        let mut parser = IncrementalCsv::new();
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            // 0xFF is invalid UTF-8 but contains no NUL byte.
            f.write_all(b"1,\xFFbad\n").unwrap();
        }
        let alert = poll_step(&config, &mut offset, &mut parser).unwrap();
        assert_eq!(alert.unwrap_err(), FollowAlertKind::EncodingChanged);
    }

    // NTFS creation-time "tunneling" can preserve the created stamp when a
    // same-named file is recreated quickly, so the identity check is
    // best-effort on Windows (backstopped by the size check); the inode
    // comparison is exact — assert it where it exists.
    #[cfg(unix)]
    #[test]
    fn rotation_with_equal_or_larger_size_is_detected_by_identity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log.csv");
        std::fs::write(&path, "a,b\n1,2\n").unwrap();
        let start = std::fs::metadata(&path).unwrap().len();
        let config = WatcherConfig {
            doc_id: 1,
            path: path.clone(),
            start_offset: start,
            delimiter: b',',
            n_cols: 2,
            identity: identity_of(&path),
            require_utf8: true,
        };
        let mut offset = start;
        let mut parser = IncrementalCsv::new();

        // Rotate: replace the file with a NEW one that is already LARGER
        // than the old offset — the size check alone would read through it.
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, "a,b\nfresh,rows\nfresh,rows\nfresh,rows\n").unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() >= start);

        let alert = poll_step(&config, &mut offset, &mut parser).unwrap();
        assert_eq!(alert.unwrap_err(), FollowAlertKind::TruncatedOrRotated);
    }
}
