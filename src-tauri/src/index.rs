//! F10: streaming record index for huge files opened in read-only mode.
//!
//! Instead of materialising every cell as a `String`, an indexed document
//! keeps only a table of record-start byte offsets (8 bytes per row) plus a
//! file handle. The source is scanned once in fixed-size chunks with a
//! quote-state-aware state machine (RFC 4180: `""` escapes inside quoted
//! fields, quotes only open a quoted field at field start), so embedded
//! newlines can never corrupt offsets. Reads then seek + parse just the
//! requested records with the same `csv` parser the in-memory path uses.
//!
//! Non-UTF-8 sources (UTF-16, Windows-1252, …) are streaming-decoded ONCE
//! into a UTF-8 cache file under the app cache dir; the index refers to the
//! cache. Each cache directory holds a held-open `lock` file while its owner
//! process lives, so [`sweep_stale`] can safely delete directories left
//! behind by an abnormal termination.

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use encoding_rs::{Encoding, UTF_8};
use serde::Serialize;

use crate::dto::FileFingerprint;
use crate::error::{AppError, AppResult};
use crate::parse::{ImportInfo, RaggedSample};
use crate::{delimiter, encoding, util};

/// Bytes read from the source per scan step (tests shrink this to force
/// records to straddle chunk boundaries).
const DEFAULT_CHUNK_SIZE: usize = 1024 * 1024;
/// Records materialised at a time during sequential visits.
const VISIT_BLOCK: usize = 4096;
/// Scattered reads coalesce indices whose gap is at most this many records
/// into one contiguous read.
const COALESCE_GAP: usize = 64;
/// Ragged-record samples retained per distinct field count (the merged result
/// is capped to [`RAGGED_SAMPLE_LIMIT`], matching `parse.rs`).
const RAGGED_SAMPLE_LIMIT: usize = 1000;
/// Bytes sampled by [`estimate`] to extrapolate row count and memory.
const PROBE_BYTES: usize = 4 * 1024 * 1024;

/// Estimated in-memory size above which the open flow asks the user to choose
/// a mode (1 GiB).
pub const MEMORY_DECISION_THRESHOLD: u64 = 1024 * 1024 * 1024;
/// Raw file size above which the open flow always asks, regardless of the
/// estimate (512 MiB).
pub const SIZE_DECISION_THRESHOLD: u64 = 512 * 1024 * 1024;

// ----- scanning state machine ---------------------------------------------------

/// Streaming record-boundary scanner. Fed chunks of the (UTF-8) data bytes;
/// quote and CRLF state carry across chunk boundaries.
struct Scanner {
    delim: u8,
    pos: u64,
    /// 1-based physical line number of the current position (embedded quoted
    /// newlines advance it, matching the `csv` crate's line accounting).
    line: u64,
    in_quotes: bool,
    /// The previous byte closed a quoted section (`"` seen while in quotes);
    /// a `"` right after re-opens it (the `""` escape).
    quote_closed: bool,
    at_field_start: bool,
    pending_cr: bool,
    record_start: u64,
    record_line: u64,
    record_delims: usize,
    record_has_bytes: bool,
    saw_crlf: bool,
    offsets: Vec<u64>,
    /// field-count histogram over all records (small: one entry per distinct
    /// field count).
    histogram: HashMap<usize, usize>,
    /// First [`RAGGED_SAMPLE_LIMIT`] `(line, fields)` per distinct field
    /// count, so ragged samples can be reconstructed once the modal count is
    /// known without retaining every record's shape.
    shape_samples: HashMap<usize, Vec<(u64, usize)>>,
}

impl Scanner {
    fn new(delim: u8, start_offset: u64) -> Scanner {
        Scanner {
            delim,
            pos: start_offset,
            line: 1,
            in_quotes: false,
            quote_closed: false,
            at_field_start: true,
            pending_cr: false,
            record_start: start_offset,
            record_line: 1,
            record_delims: 0,
            record_has_bytes: false,
            saw_crlf: false,
            offsets: Vec::new(),
            histogram: HashMap::new(),
            shape_samples: HashMap::new(),
        }
    }

    fn emit_record(&mut self) {
        let fields = self.record_delims + 1;
        self.offsets.push(self.record_start);
        *self.histogram.entry(fields).or_insert(0) += 1;
        let bucket = self.shape_samples.entry(fields).or_default();
        if bucket.len() < RAGGED_SAMPLE_LIMIT {
            bucket.push((self.record_line, fields));
        }
    }

    fn feed(&mut self, chunk: &[u8]) {
        for &b in chunk {
            let was_pending_cr = self.pending_cr;
            self.pending_cr = false;
            let was_quote_closed = self.quote_closed;
            self.quote_closed = false;

            if was_pending_cr && b == b'\n' {
                // The LF half of a CRLF terminator (or of an embedded CRLF):
                // already handled at the CR; just move past it.
                self.saw_crlf = true;
                self.pos += 1;
                if !self.in_quotes && !self.record_has_bytes {
                    self.record_start = self.pos;
                }
                continue;
            }

            if self.in_quotes {
                match b {
                    b'"' => {
                        self.in_quotes = false;
                        self.quote_closed = true;
                    }
                    b'\n' => self.line += 1,
                    b'\r' => {
                        self.line += 1;
                        self.pending_cr = true;
                    }
                    _ => {}
                }
                self.pos += 1;
                continue;
            }

            match b {
                b'"' if was_quote_closed => {
                    // `""` escape: back inside the quoted field.
                    self.in_quotes = true;
                    self.record_has_bytes = true;
                }
                b'"' if self.at_field_start => {
                    self.in_quotes = true;
                    self.at_field_start = false;
                    self.record_has_bytes = true;
                }
                b'"' => {
                    // A quote in the middle of an unquoted field is literal
                    // content (lenient, like the csv crate).
                    self.record_has_bytes = true;
                }
                b'\r' | b'\n' => {
                    if self.record_has_bytes {
                        self.emit_record();
                    }
                    self.line += 1;
                    self.pending_cr = b == b'\r';
                    self.record_start = self.pos + 1;
                    self.record_line = self.line;
                    self.record_delims = 0;
                    self.record_has_bytes = false;
                    self.at_field_start = true;
                }
                d if d == self.delim => {
                    self.record_delims += 1;
                    self.record_has_bytes = true;
                    self.at_field_start = true;
                }
                _ => {
                    self.record_has_bytes = true;
                    self.at_field_start = false;
                }
            }
            self.pos += 1;
        }
    }

    /// Flush the final record (no trailing newline) and summarise shapes.
    /// Returns `(offsets, data_len, saw_crlf, import, max_fields)`.
    fn finish(mut self) -> (Vec<u64>, u64, bool, ImportInfo, usize) {
        if self.record_has_bytes || self.in_quotes {
            self.emit_record();
        }
        let max_fields = self.histogram.keys().copied().max().unwrap_or(0);
        // Modal count; ties resolve toward the larger count (parse.rs rule).
        let modal_field_count = self
            .histogram
            .iter()
            .max_by_key(|&(count, freq)| (*freq, *count))
            .map(|(count, _)| *count)
            .unwrap_or(0);
        let total: usize = self.histogram.values().sum();
        let ragged_total = total - self.histogram.get(&modal_field_count).unwrap_or(&0);

        let mut ragged: Vec<(u64, usize)> = self
            .shape_samples
            .into_iter()
            .filter(|&(fields, _)| fields != modal_field_count)
            .flat_map(|(_, bucket)| bucket)
            .collect();
        ragged.sort_unstable();
        ragged.truncate(RAGGED_SAMPLE_LIMIT);

        let import = ImportInfo {
            had_decode_errors: false, // caller overrides
            ragged_total,
            ragged_samples: ragged
                .into_iter()
                .map(|(line, fields)| RaggedSample { line, fields })
                .collect(),
            modal_field_count,
        };
        (self.offsets, self.pos, self.saw_crlf, import, max_fields)
    }
}

// ----- cache-directory lifecycle -------------------------------------------------

/// Owns one index cache directory. The `lock` file inside is held open (and
/// OS-locked) for the guard's lifetime; dropping the guard releases the lock
/// and removes the directory (best-effort).
pub struct IndexDirGuard {
    dir: PathBuf,
    lock: Option<File>,
}

impl IndexDirGuard {
    /// Create a fresh, uniquely named cache directory under `root` and take
    /// its lock.
    fn create(root: &Path) -> AppResult<IndexDirGuard> {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        fs::create_dir_all(root)?;
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0);
        let dir = root.join(format!(
            "idx-{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
            nanos
        ));
        fs::create_dir_all(&dir)?;
        let lock = File::create(dir.join("lock"))?;
        // Advisory (POSIX) / mandatory (Windows) exclusive lock marks the
        // directory as owned by a live process.
        if lock.try_lock().is_err() {
            return Err(AppError::Other("index cache directory is locked".into()));
        }
        Ok(IndexDirGuard {
            dir,
            lock: Some(lock),
        })
    }

    fn cache_file(&self) -> PathBuf {
        self.dir.join("cache.csv")
    }
}

impl Drop for IndexDirGuard {
    fn drop(&mut self) {
        // Release the lock before deleting (Windows cannot delete an open file).
        self.lock.take();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// Delete index cache directories whose owning process is gone (their `lock`
/// file can be exclusively locked). Called once at startup; directories in
/// use by another running instance are skipped.
pub fn sweep_stale(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let lock_path = dir.join("lock");
        let owned_elsewhere = match File::open(&lock_path) {
            Ok(file) => file.try_lock().is_err(), // still locked -> owner alive
            // No lock file at all: not one of ours or half-created; treat a
            // missing lock as stale, anything else (permission) as in use.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => true,
        };
        if !owned_elsewhere {
            let _ = fs::remove_dir_all(&dir);
        }
    }
}

// ----- building the index ---------------------------------------------------------

/// Overrides for indexing; `None` means auto-detect (same knobs as
/// [`crate::parse::ParseSettings`] plus the header toggle).
#[derive(Default)]
pub struct IndexSettings {
    pub delimiter: Option<u8>,
    pub encoding: Option<&'static Encoding>,
    pub has_header_row: Option<bool>,
    /// Scan chunk size override for tests (0 = default 1 MiB).
    pub chunk_size: usize,
}

/// Everything a [`crate::document::Document`] needs to present an indexed file.
pub struct IndexedFile {
    pub handle: IndexHandle,
    pub headers: Vec<String>,
    pub has_header_row: bool,
    pub encoding_name: String,
    pub had_bom: bool,
    pub uses_crlf: bool,
    pub import: ImportInfo,
}

/// Scan `source` and build its record index, streaming the file in chunks.
/// Non-UTF-8 sources are transcoded into a UTF-8 cache file under
/// `cache_root`. `progress` receives byte deltas of the SOURCE file and may
/// fail to cancel the scan (partial cache files are cleaned up by the guard).
pub fn build_index(
    source: &Path,
    cache_root: &Path,
    settings: &IndexSettings,
    progress: &mut dyn FnMut(u64) -> AppResult<()>,
) -> AppResult<IndexedFile> {
    let chunk_size = if settings.chunk_size == 0 {
        DEFAULT_CHUNK_SIZE
    } else {
        settings.chunk_size
    };
    let mut file = File::open(source)?;

    // ---- probe: encoding + delimiter from the first chunk -----------------
    let mut probe = vec![0u8; chunk_size.max(64 * 1024)];
    let mut probe_len = 0usize;
    while probe_len < probe.len() {
        let n = file.read(&mut probe[probe_len..])?;
        if n == 0 {
            break;
        }
        probe_len += n;
    }
    probe.truncate(probe_len);

    let (enc, had_bom) = match settings.encoding {
        Some(e) => {
            let had_bom = Encoding::for_bom(&probe)
                .map(|(bom_enc, _)| bom_enc == e)
                .unwrap_or(false);
            (e, had_bom)
        }
        None => encoding::detect(&probe),
    };
    let (probe_text, _) = encoding::decode(&probe, enc);
    if probe_text.as_bytes().iter().take(8192).any(|&b| b == 0) {
        return Err(AppError::invalid(
            "this does not look like a delimited text file",
        ));
    }
    let delim = settings
        .delimiter
        .unwrap_or_else(|| delimiter::detect(&probe_text));

    // ---- full scan ---------------------------------------------------------
    file.seek(SeekFrom::Start(0))?;
    let utf8_direct = enc == UTF_8;
    let bom_len = if had_bom {
        Encoding::for_bom(&probe).map(|(_, len)| len).unwrap_or(0)
    } else {
        0
    };

    let mut had_decode_errors = false;
    let mut buf = vec![0u8; chunk_size];

    let (offsets, data_len, saw_crlf, mut import, n_cols, guard) = if utf8_direct {
        // Offsets point into the source file itself; records decode lossily
        // on read, so damage is tolerated without a cache copy.
        let mut scanner = Scanner::new(delim, bom_len as u64);
        let mut checker = Utf8Checker::default();
        file.seek(SeekFrom::Start(bom_len as u64))?;
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            checker.feed(&buf[..n]);
            scanner.feed(&buf[..n]);
            progress(n as u64)?;
        }
        had_decode_errors = checker.finish();
        let (offsets, data_len, saw_crlf, import, n_cols) = scanner.finish();
        (offsets, data_len, saw_crlf, import, n_cols, None)
    } else {
        // Streaming-decode into the UTF-8 cache, scanning the decoded bytes.
        let guard = IndexDirGuard::create(cache_root)?;
        let cache_path = guard.cache_file();
        let mut writer = BufWriter::new(File::create(&cache_path)?);
        let mut decoder = enc.new_decoder(); // strips a BOM itself
        let mut scanner = Scanner::new(delim, 0);
        let mut decoded = String::with_capacity(chunk_size + 16);
        loop {
            let n = file.read(&mut buf)?;
            let last = n == 0;
            let mut input = &buf[..n];
            loop {
                decoded.clear();
                let (result, read, had_errors) =
                    decoder.decode_to_string(input, &mut decoded, last);
                had_decode_errors |= had_errors;
                input = &input[read..];
                scanner.feed(decoded.as_bytes());
                writer.write_all(decoded.as_bytes())?;
                if result == encoding_rs::CoderResult::InputEmpty {
                    break;
                }
            }
            progress(n as u64)?;
            if last {
                break;
            }
        }
        writer.flush()?;
        let (offsets, data_len, saw_crlf, import, n_cols) = scanner.finish();
        (offsets, data_len, saw_crlf, import, n_cols, Some(guard))
    };
    import.had_decode_errors = had_decode_errors;

    if offsets.is_empty() {
        return Err(AppError::invalid("the file contains no records"));
    }

    let data_path = guard
        .as_ref()
        .map(|g| g.cache_file())
        .unwrap_or_else(|| source.to_path_buf());
    // Direct-read indexes remember the source's identity as of the scan so
    // window reads can detect ANY later rewrite (see `source_check`).
    let source_check = if guard.is_none() {
        util::stat_fingerprint(source)
    } else {
        None
    };

    let mut handle = IndexHandle {
        data_path,
        offsets,
        data_len,
        delimiter: delim,
        n_cols,
        first_data: 0,
        source_check,
        guard,
    };

    // ---- header row --------------------------------------------------------
    let first = handle.read_records_abs(0, 1)?;
    let first_record = first.first().cloned().unwrap_or_default();
    let has_header_row = settings
        .has_header_row
        .unwrap_or_else(|| util::looks_like_header(&first_record));
    let headers = if has_header_row {
        handle.first_data = 1;
        let mut h = first_record;
        h.resize(handle.n_cols, String::new());
        h
    } else {
        (0..handle.n_cols)
            .map(|i| format!("Column {}", i + 1))
            .collect()
    };

    Ok(IndexedFile {
        handle,
        headers,
        has_header_row,
        encoding_name: enc.name().to_string(),
        had_bom,
        uses_crlf: saw_crlf,
        import,
    })
}

/// Incremental UTF-8 validity check with carry-over of an incomplete trailing
/// sequence between chunks. Only used to FLAG damage (reads decode lossily).
#[derive(Default)]
struct Utf8Checker {
    carry: Vec<u8>,
    invalid: bool,
}

impl Utf8Checker {
    fn feed(&mut self, chunk: &[u8]) {
        if self.invalid {
            return;
        }
        // The carry is at most 3 bytes (a valid prefix of one multi-byte
        // character), so this buffer is chunk-sized at worst.
        let mut data = std::mem::take(&mut self.carry);
        data.extend_from_slice(chunk);
        if let Err(e) = std::str::from_utf8(&data) {
            if e.error_len().is_some() {
                self.invalid = true;
            } else {
                // Incomplete sequence at the end: carry it into the next chunk.
                self.carry = data[e.valid_up_to()..].to_vec();
            }
        }
    }

    fn finish(self) -> bool {
        self.invalid || !self.carry.is_empty()
    }
}

// ----- the handle ------------------------------------------------------------------

/// A built record index: offsets into `data_path` (the UTF-8 source or the
/// UTF-8 cache). Cheap to hold; every read opens, seeks and parses just the
/// requested window.
pub struct IndexHandle {
    data_path: PathBuf,
    /// Absolute start offset of every record, in order (header included).
    offsets: Vec<u64>,
    data_len: u64,
    delimiter: u8,
    n_cols: usize,
    /// Index of the first DATA record (1 when a header row exists).
    first_data: usize,
    /// Identity of `data_path` as of the scan, when the index reads the
    /// SOURCE file directly (UTF-8 path). Every window read re-validates it,
    /// so an in-place rewrite — even one that keeps the length — errors
    /// instead of slicing stale offsets over new bytes. `None` for the cache
    /// path (the cache file is private to this process).
    source_check: Option<FileFingerprint>,
    /// Keeps the UTF-8 cache directory (when one exists) alive for the
    /// handle's lifetime; dropping the handle deletes it.
    #[allow(dead_code)] // held for its Drop effect
    guard: Option<IndexDirGuard>,
}

impl IndexHandle {
    pub fn n_data_records(&self) -> usize {
        self.offsets.len() - self.first_data
    }

    #[cfg(test)]
    pub fn n_cols(&self) -> usize {
        self.n_cols
    }

    pub fn delimiter(&self) -> u8 {
        self.delimiter
    }

    /// Whether this index reads through a transcoded UTF-8 cache directory.
    #[cfg(test)]
    pub fn has_cache_dir(&self) -> bool {
        self.guard.is_some()
    }

    /// Approximate memory the offset table itself occupies (for diagnostics).
    #[cfg(test)]
    pub fn index_bytes(&self) -> usize {
        self.offsets.len() * std::mem::size_of::<u64>()
    }

    /// Read data records `[start, end)` (data-record coordinates: the header
    /// record, when present, is transparent). Rows are padded to `n_cols`.
    pub fn read_records(&self, start: usize, end: usize) -> AppResult<Vec<Vec<String>>> {
        self.read_records_abs(start + self.first_data, end + self.first_data)
    }

    /// Read absolute records `[start, end)` (header included in the range).
    fn read_records_abs(&self, start: usize, end: usize) -> AppResult<Vec<Vec<String>>> {
        let end = end.min(self.offsets.len());
        if start >= end {
            return Ok(Vec::new());
        }
        let a = self.offsets[start];
        let b = if end == self.offsets.len() {
            self.data_len
        } else {
            self.offsets[end]
        };

        // A direct-read source must still be the file that was scanned: an
        // in-place rewrite that kept the length would otherwise be sliced
        // silently with stale offsets.
        if let Some(expected) = self.source_check {
            if util::stat_fingerprint(&self.data_path) != Some(expected) {
                return Err(AppError::Other(
                    "the source file changed on disk; reload the document".into(),
                ));
            }
        }

        let mut file = File::open(&self.data_path)?;
        let file_len = file.metadata()?.len();
        if file_len < b {
            return Err(AppError::Other(
                "the source file changed on disk; reload the document".into(),
            ));
        }
        file.seek(SeekFrom::Start(a))?;
        let mut window = vec![0u8; (b - a) as usize];
        file.read_exact(&mut window)?;

        let mut reader = csv::ReaderBuilder::new()
            .delimiter(self.delimiter)
            .has_headers(false)
            .flexible(true)
            .from_reader(window.as_slice());
        let mut out: Vec<Vec<String>> = Vec::with_capacity(end - start);
        let mut record = csv::ByteRecord::new();
        while reader.read_byte_record(&mut record)? {
            let mut row: Vec<String> = record
                .iter()
                .map(|field| String::from_utf8_lossy(field).into_owned())
                .collect();
            row.resize(self.n_cols, String::new());
            out.push(row);
        }
        if out.len() != end - start {
            return Err(AppError::Other(
                "the source file changed on disk; reload the document".into(),
            ));
        }
        Ok(out)
    }

    /// Visit data rows `[range)` in order, in bounded blocks. The callback's
    /// row borrow is only valid during the call; returning `Ok(false)` stops
    /// the visit early.
    pub fn visit(
        &self,
        range: Range<usize>,
        f: &mut dyn FnMut(usize, &[String]) -> AppResult<bool>,
    ) -> AppResult<()> {
        let end = range.end.min(self.n_data_records());
        let mut start = range.start.min(end);
        while start < end {
            let block_end = (start + VISIT_BLOCK).min(end);
            let rows = self.read_records(start, block_end)?;
            for (i, row) in rows.iter().enumerate() {
                if !f(start + i, row)? {
                    return Ok(());
                }
            }
            start = block_end;
        }
        Ok(())
    }

    /// Visit specific data rows in CALLER order, chunked and coalesced so
    /// nearby indices share one contiguous read.
    pub fn visit_at(
        &self,
        indices: &[usize],
        f: &mut dyn FnMut(usize, &[String]) -> AppResult<bool>,
    ) -> AppResult<()> {
        let n = self.n_data_records();
        for chunk in indices.chunks(VISIT_BLOCK) {
            if let Some(&bad) = chunk.iter().find(|&&i| i >= n) {
                return Err(AppError::invalid(format!("row {bad} is out of range")));
            }
            // Fetch each distinct row once, reading coalesced runs.
            let mut sorted: Vec<usize> = chunk.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            let mut fetched: HashMap<usize, Vec<String>> = HashMap::with_capacity(sorted.len());
            let mut run_start = 0usize;
            while run_start < sorted.len() {
                let mut run_end = run_start;
                while run_end + 1 < sorted.len()
                    && sorted[run_end + 1] - sorted[run_end] <= COALESCE_GAP
                {
                    run_end += 1;
                }
                let lo = sorted[run_start];
                let hi = sorted[run_end];
                let rows = self.read_records(lo, hi + 1)?;
                for &want in &sorted[run_start..=run_end] {
                    fetched.insert(want, rows[want - lo].clone());
                }
                run_start = run_end + 1;
            }
            for &i in chunk {
                let row = fetched.get(&i).expect("fetched above");
                if !f(i, row)? {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
}

// ----- open-time estimation --------------------------------------------------------

/// What `probe_open` reports to the UI so it can offer indexed mode.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenEstimate {
    pub file_size: u64,
    pub estimated_rows: u64,
    /// Rough bytes the fully materialised in-memory document would need.
    pub estimated_memory: u64,
    pub needs_decision: bool,
    pub encoding: String,
}

/// Sample the head of `path` and extrapolate the in-memory cost of opening it
/// editable. Over the thresholds, the open flow asks the user to choose.
pub fn estimate(path: &Path) -> AppResult<OpenEstimate> {
    let mut file = File::open(path)?;
    let file_size = file.metadata()?.len();

    let mut probe = vec![0u8; PROBE_BYTES];
    let mut len = 0usize;
    while len < probe.len() {
        let n = file.read(&mut probe[len..])?;
        if n == 0 {
            break;
        }
        len += n;
    }
    probe.truncate(len);

    let (enc, _) = encoding::detect(&probe);
    let (text, _) = encoding::decode(&probe, enc);
    let delim = delimiter::detect(&text);

    let mut scanner = Scanner::new(delim, 0);
    scanner.feed(text.as_bytes());
    let (offsets, _, _, _, max_fields) = scanner.finish();
    let sampled_fields = max_fields.max(1);
    // Drop the (possibly truncated) final sample record from the average.
    let complete_records = offsets.len().saturating_sub(1).max(1);

    let decoded_ratio = if len == 0 {
        1.0
    } else {
        text.len() as f64 / len as f64
    };
    let decoded_total = file_size as f64 * decoded_ratio;
    let avg_record_bytes = (text.len() as f64 / complete_records as f64).max(1.0);
    let estimated_rows = (decoded_total / avg_record_bytes).ceil() as u64;

    // Cell text + String headers (24B) & modest allocator slack + row Vec
    // headers. Deliberately rough: this only picks a threshold.
    let estimated_memory = (decoded_total
        + estimated_rows as f64 * sampled_fields as f64 * 40.0
        + estimated_rows as f64 * 32.0) as u64;

    Ok(OpenEstimate {
        file_size,
        estimated_rows,
        estimated_memory,
        needs_decision: estimated_memory > MEMORY_DECISION_THRESHOLD
            || file_size > SIZE_DECISION_THRESHOLD,
        encoding: enc.name().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::{parse, ParseSettings};

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ceesvee-index-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_source(root: &Path, bytes: &[u8]) -> PathBuf {
        let path = root.join("source.csv");
        fs::write(&path, bytes).unwrap();
        path
    }

    fn build(bytes: &[u8], chunk_size: usize) -> (IndexedFile, PathBuf) {
        let root = temp_root("build");
        let source = write_source(&root, bytes);
        let settings = IndexSettings {
            chunk_size,
            ..Default::default()
        };
        let indexed = build_index(&source, &root.join("indexes"), &settings, &mut |_| Ok(()))
            .expect("index builds");
        (indexed, root)
    }

    /// Golden pair: every record read through the index must equal parse.rs's
    /// in-memory result for the same bytes.
    fn assert_matches_parse(bytes: &[u8], chunk_size: usize) {
        let (indexed, root) = build(bytes, chunk_size);
        let parsed = parse(bytes, &ParseSettings::default()).unwrap();

        let n_data = indexed.handle.n_data_records();
        let expected_data = parsed.records.len() - usize::from(indexed.has_header_row);
        assert_eq!(n_data, expected_data, "record count");
        assert_eq!(indexed.handle.n_cols(), parsed.n_cols, "n_cols");

        let rows = indexed.handle.read_records(0, n_data).unwrap();
        let skip = usize::from(indexed.has_header_row);
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(row, &parsed.records[i + skip], "row {i}");
        }
        drop(indexed);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn matches_parse_for_plain_csv() {
        assert_matches_parse(b"name,qty\nApple,3\nBanana,7\n", 1024);
    }

    #[test]
    fn matches_parse_with_quoted_newlines_and_escapes() {
        let src = b"h1,h2\n\"line\nbreak\",x\n\"say \"\"hi\"\"\",y\nplain,z\n";
        assert_matches_parse(src, 1024);
    }

    #[test]
    fn quoted_newline_straddles_chunk_boundary() {
        // Chunk size 8 forces the quoted field (with its embedded newline)
        // to span several chunks; offsets must be unaffected.
        let src = b"a,b\n\"long\nquoted\nfield\",2\nlast,3\n";
        assert_matches_parse(src, 8);
        for chunk in [1usize, 3, 5, 16] {
            assert_matches_parse(src, chunk);
        }
    }

    #[test]
    fn crlf_and_missing_trailing_newline() {
        assert_matches_parse(b"a,b\r\n1,2\r\n3,4", 1024);
        let (indexed, root) = build(b"a,b\r\n1,2\r\n", 1024);
        assert!(indexed.uses_crlf);
        drop(indexed);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn empty_lines_are_skipped_like_parse() {
        assert_matches_parse(b"a,b\n1,2\n\n\n3,4\n\n", 1024);
    }

    #[test]
    fn ragged_records_pad_and_report() {
        let src = b"a,b,c\n1,2\n4,5,6\n7\n";
        assert_matches_parse(src, 1024);
        let (indexed, root) = build(src, 1024);
        assert_eq!(indexed.import.modal_field_count, 3);
        assert_eq!(indexed.import.ragged_total, 2);
        assert_eq!(
            indexed.import.ragged_samples,
            vec![
                RaggedSample { line: 2, fields: 2 },
                RaggedSample { line: 4, fields: 1 },
            ]
        );
        drop(indexed);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ragged_line_numbers_account_for_quoted_newlines() {
        let src = b"a,b\n\"x\ny\",2\n3";
        assert_matches_parse(src, 4);
        let (indexed, root) = build(src, 4);
        assert_eq!(
            indexed.import.ragged_samples,
            vec![RaggedSample { line: 4, fields: 1 }]
        );
        drop(indexed);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn utf16le_source_goes_through_cache() {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "name,qty\ncafé,3\nnaïve,7\n".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let root = temp_root("utf16");
        let source = write_source(&root, &bytes);
        let indexed = build_index(
            &source,
            &root.join("indexes"),
            &IndexSettings::default(),
            &mut |_| Ok(()),
        )
        .unwrap();
        assert_eq!(indexed.encoding_name, "UTF-16LE");
        assert!(indexed.had_bom);
        assert!(indexed.handle.has_cache_dir(), "uses a cache dir");
        let rows = indexed.handle.read_records(0, 2).unwrap();
        assert_eq!(rows[0], vec!["café", "3"]);
        assert_eq!(rows[1], vec!["naïve", "7"]);
        drop(indexed);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn windows_1252_source_goes_through_cache() {
        // "café,1\nnaïve,2\n" in Windows-1252 (é=0xE9, ï=0xEF).
        let bytes = b"name,qty\ncaf\xE9,1\nna\xEFve,2\n";
        let root = temp_root("w1252");
        let source = write_source(&root, bytes);
        let indexed = build_index(
            &source,
            &root.join("indexes"),
            &IndexSettings {
                encoding: Some(encoding_rs::WINDOWS_1252),
                ..Default::default()
            },
            &mut |_| Ok(()),
        )
        .unwrap();
        let rows = indexed.handle.read_records(0, 2).unwrap();
        assert_eq!(rows[0], vec!["café", "1"]);
        assert_eq!(rows[1], vec!["naïve", "2"]);
        drop(indexed);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn utf8_source_reads_directly_without_cache() {
        let (indexed, root) = build("name\nsmörgås\n".as_bytes(), 1024);
        assert!(!indexed.handle.has_cache_dir(), "no cache dir for UTF-8");
        assert_eq!(indexed.handle.data_path.file_name().unwrap(), "source.csv");
        let rows = indexed.handle.read_records(0, 1).unwrap();
        assert_eq!(rows[0], vec!["smörgås"]);
        drop(indexed);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn utf8_bom_offsets_skip_bom() {
        let src = b"\xEF\xBB\xBFa,b\n1,2\n";
        let (indexed, root) = build(src, 1024);
        assert!(indexed.had_bom);
        assert_eq!(indexed.headers, vec!["a", "b"]);
        let rows = indexed.handle.read_records(0, 1).unwrap();
        assert_eq!(rows[0], vec!["1", "2"]);
        drop(indexed);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_utf8_is_flagged_and_read_lossily() {
        // Force UTF-8 (like parse.rs's equivalent test): 0xFF is invalid, so
        // the damage flag must be set and reads must substitute U+FFFD.
        let src = b"a,b\n\xFFbad,2\n";
        let root = temp_root("damage");
        let source = write_source(&root, src);
        let indexed = build_index(
            &source,
            &root.join("indexes"),
            &IndexSettings {
                encoding: Some(encoding_rs::UTF_8),
                ..Default::default()
            },
            &mut |_| Ok(()),
        )
        .unwrap();
        assert!(indexed.import.had_decode_errors);
        assert!(!indexed.handle.has_cache_dir(), "UTF-8 stays direct");
        let rows = indexed.handle.read_records(0, 1).unwrap();
        assert!(rows[0][0].contains('\u{FFFD}'));
        drop(indexed);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn windowed_reads_return_exactly_requested_records() {
        let mut src = String::from("h\n");
        for i in 0..500 {
            src.push_str(&format!("row-{i}\n"));
        }
        let (indexed, root) = build(src.as_bytes(), 64);
        let rows = indexed.handle.read_records(100, 110).unwrap();
        assert_eq!(rows.len(), 10);
        assert_eq!(rows[0], vec!["row-100"]);
        assert_eq!(rows[9], vec!["row-109"]);
        // Range clamped to the end.
        let tail = indexed.handle.read_records(498, 600).unwrap();
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[1], vec!["row-499"]);
        drop(indexed);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn visit_streams_all_rows_in_order_and_can_stop() {
        let mut src = String::from("h\n");
        for i in 0..1000 {
            src.push_str(&format!("{i}\n"));
        }
        let (indexed, root) = build(src.as_bytes(), 256);
        let mut seen = Vec::new();
        indexed
            .handle
            .visit(0..1000, &mut |i, row| {
                seen.push((i, row[0].clone()));
                Ok(true)
            })
            .unwrap();
        assert_eq!(seen.len(), 1000);
        assert_eq!(seen[0], (0, "0".into()));
        assert_eq!(seen[999], (999, "999".into()));

        let mut count = 0;
        indexed
            .handle
            .visit(0..1000, &mut |_, _| {
                count += 1;
                Ok(count < 5)
            })
            .unwrap();
        assert_eq!(count, 5, "early exit stops the visit");
        drop(indexed);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn visit_at_preserves_caller_order_and_coalesces() {
        let mut src = String::from("h\n");
        for i in 0..300 {
            src.push_str(&format!("{i}\n"));
        }
        let (indexed, root) = build(src.as_bytes(), 128);
        let want = [250usize, 3, 4, 251, 0, 299];
        let mut seen = Vec::new();
        indexed
            .handle
            .visit_at(&want, &mut |i, row| {
                seen.push((i, row[0].clone()));
                Ok(true)
            })
            .unwrap();
        assert_eq!(
            seen,
            want.iter().map(|&i| (i, i.to_string())).collect::<Vec<_>>()
        );
        assert!(indexed
            .handle
            .visit_at(&[300], &mut |_, _| Ok(true))
            .is_err());
        drop(indexed);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn same_length_rewrite_errors_instead_of_stale_slices() {
        // An in-place rewrite that KEEPS the byte length must still be
        // detected: stale offsets over new bytes would silently corrupt
        // every window read.
        let root = temp_root("rewrite");
        let source = write_source(&root, b"a\n111\n222\n");
        let indexed = build_index(
            &source,
            &root.join("indexes"),
            &IndexSettings::default(),
            &mut |_| Ok(()),
        )
        .unwrap();
        assert_eq!(indexed.handle.read_records(0, 1).unwrap()[0], vec!["111"]);
        fs::write(&source, b"a\n333\n444\n").unwrap();
        // Force a different mtime explicitly — same-millisecond rewrites are
        // real on fast filesystems and would make this test flaky.
        let file = fs::OpenOptions::new().write(true).open(&source).unwrap();
        file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_millis(50))
            .unwrap();
        drop(file);
        assert!(
            indexed.handle.read_records(0, 1).is_err(),
            "reads must reject a rewritten source even at the same length"
        );
        drop(indexed);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn shrunk_file_errors_instead_of_garbage() {
        let root = temp_root("shrink");
        let source = write_source(&root, b"a\n1\n2\n3\n");
        let indexed = build_index(
            &source,
            &root.join("indexes"),
            &IndexSettings::default(),
            &mut |_| Ok(()),
        )
        .unwrap();
        fs::write(&source, b"a\n1\n").unwrap();
        assert!(indexed.handle.read_records(0, 3).is_err());
        drop(indexed);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cancellation_propagates_and_cleans_cache() {
        let mut bytes = vec![0xFF, 0xFE];
        for unit in "a,b\n1,2\n3,4\n".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        let root = temp_root("cancel");
        let source = write_source(&root, &bytes);
        let indexes = root.join("indexes");
        let mut calls = 0;
        let result = build_index(
            &source,
            &indexes,
            &IndexSettings {
                chunk_size: 4,
                ..Default::default()
            },
            &mut |_| {
                calls += 1;
                if calls >= 2 {
                    Err(AppError::Cancelled)
                } else {
                    Ok(())
                }
            },
        );
        assert!(matches!(result, Err(AppError::Cancelled)));
        // The guard dropped on the error path, so no cache dirs remain.
        let leftover = fs::read_dir(&indexes).map(|d| d.count()).unwrap_or(0);
        assert_eq!(leftover, 0, "cache directory cleaned up on cancel");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn guard_drop_removes_cache_dir() {
        let root = temp_root("guard");
        let guard = IndexDirGuard::create(&root.join("indexes")).unwrap();
        let dir = guard.dir.clone();
        assert!(dir.is_dir());
        drop(guard);
        assert!(!dir.exists(), "guard removes its directory");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sweep_removes_stale_but_keeps_locked_dirs() {
        let root = temp_root("sweep");
        let indexes = root.join("indexes");

        // Stale: a directory whose lock is not held by anyone.
        let stale = indexes.join("idx-stale");
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join("lock"), b"").unwrap();
        fs::write(stale.join("cache.csv"), b"a\n1\n").unwrap();

        // Live: a guard currently holds its lock.
        let live = IndexDirGuard::create(&indexes).unwrap();

        sweep_stale(&indexes);
        assert!(!stale.exists(), "stale dir removed");
        assert!(live.dir.is_dir(), "live dir kept");
        drop(live);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn estimate_extrapolates_and_flags_decisions() {
        let root = temp_root("estimate");
        let mut src = String::from("name,qty,price\n");
        for i in 0..2000 {
            src.push_str(&format!("item-{i},{},{}.50\n", i % 100, i % 10));
        }
        let source = write_source(&root, src.as_bytes());
        let est = estimate(&source).unwrap();
        assert_eq!(est.file_size, src.len() as u64);
        // The whole file fit in the probe, so the row estimate is near-exact
        // (allowing the truncated-final-record adjustment).
        assert!(
            (est.estimated_rows as i64 - 2001).abs() <= 2,
            "estimated {} rows",
            est.estimated_rows
        );
        assert!(est.estimated_memory > est.file_size);
        assert!(!est.needs_decision, "small file needs no decision");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn bounded_memory_scan_of_many_rows() {
        // A wide-ish synthetic file with 200k rows: the index must hold only
        // offsets (8B/record), not cells. We can't measure RSS portably in a
        // unit test, but we can assert the handle's own footprint is exactly
        // the offset table and that scanning visited everything.
        let mut src = String::with_capacity(6 * 1024 * 1024);
        src.push_str("a,b,c\n");
        for i in 0..200_000 {
            src.push_str(&format!("{i},x{i},y{i}\n"));
        }
        let (indexed, root) = build(src.as_bytes(), DEFAULT_CHUNK_SIZE);
        assert_eq!(indexed.handle.n_data_records(), 200_000);
        assert_eq!(
            indexed.handle.index_bytes(),
            (200_000 + 1) * 8,
            "index stores offsets only"
        );
        let rows = indexed.handle.read_records(199_998, 200_000).unwrap();
        assert_eq!(rows[1], vec!["199999", "x199999", "y199999"]);
        drop(indexed);
        let _ = fs::remove_dir_all(root);
    }
}
