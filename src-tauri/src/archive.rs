//! Compressed CSV support (F17): streaming gzip and ZIP handling.
//!
//! Archives are never edited in place. Opening extracts the chosen entry
//! into a lock-guarded cache directory (the same protocol — and startup
//! sweep — as the F10 index caches); the extracted file then flows through
//! the ordinary open pipeline (estimate → editable or indexed). Extraction
//! streams in bounded chunks with decompressed-size and compression-ratio
//! caps, so a ZIP bomb fails fast instead of filling the disk. Entry names
//! from archives are NEVER used as filesystem paths — output files always
//! get generated names inside our own cache directory, which makes path
//! traversal names inert by construction.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Serialize;

use crate::dto::FileFingerprint;
use crate::error::{AppError, AppResult};
use crate::index::IndexDirGuard;
use crate::{delimiter, encoding, util};

/// Hard cap on decompressed output (8 GiB): past this, extraction always
/// fails — even confirmed — because downstream handling would too.
pub const MAX_DECOMPRESSED_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Expansion ratio beyond which extraction requires explicit confirmation
/// (`allow_large`), once the output exceeds [`RATIO_GUARD_MIN`].
pub const SUSPICIOUS_RATIO: u64 = 200;
/// Ratio checks only kick in past this output size — tiny files compress
/// absurdly well without being bombs.
pub const RATIO_GUARD_MIN: u64 = 64 * 1024 * 1024;

const CHUNK: usize = 1024 * 1024;
/// Bytes sniffed per ZIP entry for the chooser's delimiter/encoding hints.
const SNIFF_BYTES: usize = 16 * 1024;

/// Where a document originally came from, when that was an archive.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveOrigin {
    pub archive_path: String,
    /// The ZIP entry name; `None` for plain gzip members.
    pub entry_name: Option<String>,
    /// Identity of the archive at extraction time.
    pub archive_fingerprint: Option<FileFingerprint>,
}

/// One candidate entry inside a ZIP, for the chooser dialog.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZipEntryInfo {
    pub name: String,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    /// Uncompressed / compressed (1 when incompressible or empty).
    pub ratio: f64,
    pub encrypted: bool,
    /// Best-effort sniffs from the entry's first bytes (None if unreadable).
    pub likely_delimiter: Option<String>,
    pub likely_encoding: Option<String>,
}

/// An extracted-but-not-yet-opened archive entry, parked between the
/// extraction job and the user's editable/indexed decision. Dropping it
/// (discard, replace, app exit) deletes the cache directory via the guard.
pub struct PendingArchive {
    pub guard: IndexDirGuard,
    pub data_path: PathBuf,
    pub origin: ArchiveOrigin,
}

/// Pending extractions keyed by token, managed by Tauri. `Arc`-backed so
/// extraction jobs can fulfil their reserved token from the blocking pool.
#[derive(Default, Clone)]
pub struct ArchiveCache {
    inner: std::sync::Arc<ArchiveCacheInner>,
}

#[derive(Default)]
pub struct ArchiveCacheInner {
    pending: Mutex<HashMap<u64, PendingArchive>>,
    next_token: AtomicU64,
}

impl ArchiveCache {
    /// Reserve a token up front so the starting command can hand it to the
    /// front end before the extraction job finishes.
    pub fn reserve(&self) -> u64 {
        self.inner.next_token.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Store the finished extraction under its reserved token.
    pub fn fulfill(&self, token: u64, pending: PendingArchive) {
        if let Ok(mut map) = self.inner.pending.lock() {
            map.insert(token, pending);
        }
    }

    pub fn take(&self, token: u64) -> Option<PendingArchive> {
        self.inner.pending.lock().ok()?.remove(&token)
    }

    /// The extracted file's path, without consuming the pending entry.
    pub fn data_path(&self, token: u64) -> Option<PathBuf> {
        Some(
            self.inner
                .pending
                .lock()
                .ok()?
                .get(&token)?
                .data_path
                .clone(),
        )
    }

    /// Drop a pending extraction (deletes its cache directory).
    pub fn discard(&self, token: u64) {
        if let Ok(mut map) = self.inner.pending.lock() {
            map.remove(&token);
        }
    }
}

/// Whether a path looks like a gzip-compressed delimited file.
pub fn is_gzip_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("gz"))
}

/// Whether a path looks like a ZIP archive.
pub fn is_zip_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}

/// Copy `reader` into `dest` in bounded chunks, enforcing the decompression
/// caps and reporting progress through `on_input` (source-side deltas come
/// from the caller's own accounting; this reports OUTPUT bytes).
fn stream_with_caps(
    mut reader: impl Read,
    dest: &Path,
    compressed_size: u64,
    allow_large: bool,
    progress: &mut dyn FnMut(u64) -> AppResult<()>,
) -> AppResult<u64> {
    let mut writer = BufWriter::new(File::create(dest)?);
    let mut buf = vec![0u8; CHUNK];
    let mut total: u64 = 0;
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| AppError::Other(format!("decompression failed: {e}")))?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > MAX_DECOMPRESSED_BYTES {
            return Err(AppError::invalid(format!(
                "decompressed output exceeds the {} GiB safety cap",
                MAX_DECOMPRESSED_BYTES / (1024 * 1024 * 1024)
            )));
        }
        if !allow_large && total > RATIO_GUARD_MIN && compressed_size > 0 {
            let ratio = total / compressed_size.max(1);
            if ratio > SUSPICIOUS_RATIO {
                return Err(AppError::invalid(format!(
                    "suspicious compression ratio (over {SUSPICIOUS_RATIO}:1) — \
                     this can indicate a decompression bomb; confirm to continue"
                )));
            }
        }
        writer.write_all(&buf[..n])?;
        progress(n as u64)?;
    }
    writer.flush()?;
    Ok(total)
}

/// Extract a gzip member into `dir` as `data.csv`. Progress reports OUTPUT
/// bytes (the input size is known; output total is not, so the UI shows an
/// indeterminate-total bar fed by decompressed bytes).
pub fn extract_gzip(
    source: &Path,
    dir: &Path,
    allow_large: bool,
    progress: &mut dyn FnMut(u64) -> AppResult<()>,
) -> AppResult<PathBuf> {
    let compressed = std::fs::metadata(source)?.len();
    let reader = flate2::read::GzDecoder::new(BufReader::new(File::open(source)?));
    let dest = dir.join("data.csv");
    stream_with_caps(reader, &dest, compressed, allow_large, progress)?;
    Ok(dest)
}

/// List the entries of a ZIP archive for the chooser. Directories are
/// skipped; encrypted entries are listed (flagged) but cannot be opened.
pub fn list_zip_entries(path: &Path) -> AppResult<Vec<ZipEntryInfo>> {
    let mut archive = zip::ZipArchive::new(BufReader::new(File::open(path)?))
        .map_err(|e| AppError::invalid(format!("not a readable ZIP archive: {e}")))?;
    let mut out = Vec::new();
    for i in 0..archive.len() {
        // Raw access first: metadata without decompressing (and without
        // password errors for encrypted entries).
        let (name, compressed_size, uncompressed_size, encrypted, is_dir) = {
            let entry = archive
                .by_index_raw(i)
                .map_err(|e| AppError::invalid(format!("unreadable ZIP entry: {e}")))?;
            (
                entry.name().to_string(),
                entry.compressed_size(),
                entry.size(),
                entry.encrypted(),
                entry.is_dir(),
            )
        };
        if is_dir {
            continue;
        }
        // Sniff the first bytes for delimiter/encoding hints (best-effort).
        let (likely_delimiter, likely_encoding) = if encrypted {
            (None, None)
        } else {
            match archive.by_index(i) {
                Ok(mut entry) => {
                    let mut head = vec![0u8; SNIFF_BYTES];
                    let mut filled = 0usize;
                    while filled < head.len() {
                        match entry.read(&mut head[filled..]) {
                            Ok(0) => break,
                            Ok(n) => filled += n,
                            Err(_) => break,
                        }
                    }
                    head.truncate(filled);
                    if head.is_empty() {
                        (None, None)
                    } else {
                        let (enc, _) = encoding::detect(&head);
                        let (text, _) = encoding::decode(&head, enc);
                        let delim = delimiter::detect(&text);
                        (
                            Some(String::from_utf8_lossy(&[delim]).to_string()),
                            Some(enc.name().to_string()),
                        )
                    }
                }
                Err(_) => (None, None),
            }
        };
        out.push(ZipEntryInfo {
            ratio: if compressed_size > 0 {
                uncompressed_size as f64 / compressed_size as f64
            } else {
                1.0
            },
            name,
            compressed_size,
            uncompressed_size,
            encrypted,
            likely_delimiter,
            likely_encoding,
        });
    }
    if out.is_empty() {
        return Err(AppError::invalid("the ZIP archive contains no files"));
    }
    Ok(out)
}

/// Extract one ZIP entry (by exact name) into `dir` as `data.csv`. The entry
/// name is only used to SELECT the entry — never as an output path.
pub fn extract_zip_entry(
    path: &Path,
    entry_name: &str,
    dir: &Path,
    allow_large: bool,
    progress: &mut dyn FnMut(u64) -> AppResult<()>,
) -> AppResult<PathBuf> {
    let mut archive = zip::ZipArchive::new(BufReader::new(File::open(path)?))
        .map_err(|e| AppError::invalid(format!("not a readable ZIP archive: {e}")))?;
    let entry = archive.by_name(entry_name).map_err(|e| match e {
        zip::result::ZipError::UnsupportedArchive(msg) if msg.contains("Password") => {
            AppError::invalid("this ZIP entry is encrypted and cannot be opened")
        }
        other => AppError::invalid(format!("cannot open ZIP entry: {other}")),
    })?;
    if entry.encrypted() {
        return Err(AppError::invalid(
            "this ZIP entry is encrypted and cannot be opened",
        ));
    }
    let compressed = entry.compressed_size();
    let dest = dir.join("data.csv");
    stream_with_caps(entry, &dest, compressed, allow_large, progress)?;
    Ok(dest)
}

/// Extract whatever `path` is (gzip member or a named ZIP entry) into a
/// fresh guarded cache directory, returning the pending handle.
pub fn extract_to_pending(
    path: &Path,
    entry_name: Option<&str>,
    cache_root: &Path,
    allow_large: bool,
    progress: &mut dyn FnMut(u64) -> AppResult<()>,
) -> AppResult<PendingArchive> {
    let guard = IndexDirGuard::create(cache_root)?;
    let data_path = if is_zip_path(path) {
        let entry = entry_name.ok_or_else(|| AppError::invalid("no ZIP entry was selected"))?;
        extract_zip_entry(path, entry, guard.dir(), allow_large, progress)?
    } else if is_gzip_path(path) {
        extract_gzip(path, guard.dir(), allow_large, progress)?
    } else {
        return Err(AppError::invalid("not a supported compressed file"));
    };
    Ok(PendingArchive {
        origin: ArchiveOrigin {
            archive_path: path.to_string_lossy().to_string(),
            entry_name: entry_name.map(str::to_string),
            archive_fingerprint: util::stat_fingerprint(path),
        },
        guard,
        data_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use zip::write::SimpleFileOptions;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ceesvee-archive-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_gzip(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.join(name);
        let mut encoder = GzEncoder::new(File::create(&path).unwrap(), Compression::default());
        encoder.write_all(content).unwrap();
        encoder.finish().unwrap();
        path
    }

    fn write_zip(dir: &Path, name: &str, entries: &[(&str, &[u8])]) -> PathBuf {
        let path = dir.join(name);
        let mut writer = zip::ZipWriter::new(File::create(&path).unwrap());
        for (entry_name, content) in entries {
            writer
                .start_file(*entry_name, SimpleFileOptions::default())
                .unwrap();
            writer.write_all(content).unwrap();
        }
        writer.finish().unwrap();
        path
    }

    #[test]
    fn gzip_round_trips_identically() {
        let root = temp_root("gz");
        let csv = b"name,qty\nAda,3\n\"multi\nline\",7\n";
        let gz = write_gzip(&root, "data.csv.gz", csv);
        let pending =
            extract_to_pending(&gz, None, &root.join("cache"), false, &mut |_| Ok(())).unwrap();
        let extracted = std::fs::read(&pending.data_path).unwrap();
        assert_eq!(extracted, csv, "decompressed bytes identical to source");
        assert!(pending.origin.archive_fingerprint.is_some());
        let dir = pending.guard.dir().to_path_buf();
        drop(pending);
        assert!(!dir.exists(), "discard removes the cache directory");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn zip_lists_entries_with_sniffs_and_extracts_by_name() {
        let root = temp_root("zip");
        let zip_path = write_zip(
            &root,
            "bundle.zip",
            &[
                ("a.csv", b"x,y\n1,2\n".as_slice()),
                ("nested/b.tsv", b"p\tq\n3\t4\n".as_slice()),
            ],
        );
        let entries = list_zip_entries(&zip_path).unwrap();
        assert_eq!(entries.len(), 2);
        let a = entries.iter().find(|e| e.name == "a.csv").unwrap();
        assert_eq!(a.likely_delimiter.as_deref(), Some(","));
        assert!(!a.encrypted);
        let b = entries.iter().find(|e| e.name == "nested/b.tsv").unwrap();
        assert_eq!(b.likely_delimiter.as_deref(), Some("\t"));

        let pending = extract_to_pending(
            &zip_path,
            Some("nested/b.tsv"),
            &root.join("cache"),
            false,
            &mut |_| Ok(()),
        )
        .unwrap();
        assert_eq!(std::fs::read(&pending.data_path).unwrap(), b"p\tq\n3\t4\n");
        drop(pending);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn traversal_style_entry_names_never_escape_the_cache_dir() {
        let root = temp_root("traversal");
        let zip_path = write_zip(
            &root,
            "evil.zip",
            &[("../../escape.csv", b"a\n1\n".as_slice())],
        );
        let cache = root.join("cache");
        let pending = extract_to_pending(
            &zip_path,
            Some("../../escape.csv"),
            &cache,
            false,
            &mut |_| Ok(()),
        )
        .unwrap();
        // The output lives INSIDE the guarded cache dir under our own name,
        // regardless of the hostile entry name.
        assert!(pending.data_path.starts_with(&cache));
        assert!(pending.data_path.ends_with("data.csv"));
        assert!(!root.join("escape.csv").exists());
        drop(pending);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ratio_guard_blocks_suspicious_expansion_unless_confirmed() {
        let root = temp_root("bomb");
        // 128 MiB of zeros compresses tiny: ratio far above the guard.
        let zeros = vec![0u8; 130 * 1024 * 1024];
        // NUL bytes are rejected by the CSV opener later anyway; the guard
        // must trip during EXTRACTION, before any of that.
        let gz = write_gzip(&root, "bomb.csv.gz", &zeros);
        let err = match extract_to_pending(&gz, None, &root.join("cache"), false, &mut |_| Ok(())) {
            Err(e) => e,
            Ok(_) => panic!("suspicious ratio must be rejected without confirmation"),
        };
        assert!(
            err.to_string().contains("suspicious compression ratio"),
            "unexpected: {err}"
        );
        // Confirmed extraction is allowed through.
        let ok = extract_to_pending(&gz, None, &root.join("cache"), true, &mut |_| Ok(()));
        assert!(ok.is_ok());
        drop(ok);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cancellation_cleans_the_partial_cache() {
        let root = temp_root("cancel");
        let payload = vec![b'a'; 4 * 1024 * 1024];
        let gz = write_gzip(&root, "big.csv.gz", &payload);
        let cache = root.join("cache");
        let mut calls = 0;
        let result = extract_to_pending(&gz, None, &cache, false, &mut |_| {
            calls += 1;
            if calls >= 2 {
                Err(AppError::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(matches!(result, Err(AppError::Cancelled)));
        let leftover = std::fs::read_dir(&cache).map(|d| d.count()).unwrap_or(0);
        assert_eq!(leftover, 0, "guard removed the partial extraction");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn path_kind_detection() {
        assert!(is_gzip_path(Path::new("x/data.csv.GZ")));
        assert!(!is_gzip_path(Path::new("x/data.csv")));
        assert!(is_zip_path(Path::new("bundle.Zip")));
        assert!(!is_zip_path(Path::new("bundle.tar")));
    }
}
