//! The Tauri command surface. The front end drives every interaction through
//! these; heavy file I/O and parsing run off the UI thread.
//!
//! Locking model: the document registry (`Mutex<AppState>`) is held only long
//! enough to look up a document's `Arc<RwLock<Document>>`. Commands then lock
//! that single document, so long-running work on one tab never blocks the
//! others. Long scans/exports go through [`crate::job`] for progress and
//! cancellation instead of holding any lock across the whole operation.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use tauri::State;

use crate::compare::{self, CompareCache, CompareInfo, ComparePage, CompareSpec, DiffStatus};
use crate::dedup::{self, DedupCache, DedupSpec, DuplicateKeepStrategy, DuplicateReport};
use crate::diagnostics::{self, DiagnosticsCache, DiagnosticsReport};
use crate::document::Document;
use crate::dto::{
    CellRect, ColumnSummary, DocumentMeta, EncodingCompatibility, EncodingIncompatibility,
    ExportOptions, ExportScope, ExternalChange, FileFingerprint, FilterGroup, FindMatch,
    FindOptions, OpenOptions, ReparsePreview, ReplaceResult, RowsResponse, ScopeCounts,
    SelectionStats, SortKey, SplitOptions,
};
use crate::error::{AppError, AppResult};
use crate::job::JobRegistry;
use crate::parse::{parse, ParseSettings, ParsedFile};
use crate::profile::{self, ColumnProfile, ProfileCache, ProfileOptions, ProfileScope};
use crate::reopen::{self, CurrentInterpretation};
use crate::settings::{self, AppSettings, FileProfile, ProfileValidation};
use crate::state::{AppState, PendingFiles, SharedDocument};
use crate::transform::{self, TransformErrorPolicy, TransformPreview, TransformSpec};
use crate::{
    encoding, export, export_scope, filter as filter_mod, find as find_mod, save as save_mod, util,
};

type Db<'a> = State<'a, Mutex<AppState>>;

fn lock<'g>(state: &'g Db<'_>) -> AppResult<MutexGuard<'g, AppState>> {
    state
        .lock()
        .map_err(|_| AppError::Other("internal state lock error".into()))
}

/// Fetch the shared handle for a document, holding the registry lock only for
/// the lookup.
fn doc_handle(state: &Db<'_>, doc_id: u64) -> AppResult<SharedDocument> {
    lock(state)?.doc(doc_id)
}

fn poisoned<T>(_: T) -> AppError {
    AppError::Other("internal document lock error".into())
}

/// Run `f` with shared (read) access to one document.
fn read_doc<T>(
    state: &Db<'_>,
    doc_id: u64,
    f: impl FnOnce(&Document) -> AppResult<T>,
) -> AppResult<T> {
    let handle = doc_handle(state, doc_id)?;
    let doc = handle.read().map_err(poisoned)?;
    f(&doc)
}

/// Run `f` with exclusive (write) access to one document.
fn write_doc<T>(
    state: &Db<'_>,
    doc_id: u64,
    f: impl FnOnce(&mut Document) -> AppResult<T>,
) -> AppResult<T> {
    let handle = doc_handle(state, doc_id)?;
    let mut doc = handle.write().map_err(poisoned)?;
    f(&mut doc)
}

/// Translate a visible (display) row index to its absolute index, erroring if
/// out of range. Identity when no filter is active.
fn abs_row(doc: &Document, display: usize) -> AppResult<usize> {
    doc.display_to_abs(display)
        .ok_or_else(|| AppError::invalid("row index out of range"))
}

/// Like [`abs_row`] but allows a display index at the end (for append/insert).
fn abs_insert_row(doc: &Document, display: usize) -> AppResult<usize> {
    doc.display_to_abs_insert(display)
        .ok_or_else(|| AppError::invalid("row index out of range"))
}

/// Heuristic: treat the first row as a header when none of its cells is numeric.
fn looks_like_header(records: &[Vec<String>]) -> bool {
    match records.first() {
        None => false,
        Some(first) => first.iter().all(|cell| {
            let trimmed = cell.trim();
            trimmed.is_empty() || trimmed.parse::<f64>().is_err()
        }),
    }
}

/// Read and parse a file off the UI thread, also capturing its fingerprint.
async fn parse_file(
    path: std::path::PathBuf,
    delimiter: Option<u8>,
    encoding: Option<&'static encoding_rs::Encoding>,
) -> AppResult<(ParsedFile, Option<FileFingerprint>)> {
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = std::fs::read(&path)?;
        let fingerprint = util::stat_fingerprint(&path);
        let settings = ParseSettings {
            delimiter,
            encoding,
        };
        Ok((parse(&bytes, &settings)?, fingerprint))
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

// ----- open / new / close ------------------------------------------------

#[tauri::command]
pub async fn open_file(
    path: String,
    options: Option<OpenOptions>,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let options = options.unwrap_or_default();
    let opt_delim = options.delimiter.as_deref().map(util::delimiter_to_byte);
    let opt_enc = options.encoding.as_deref().map(encoding::from_name);
    let forced_header = options.has_header_row;

    let (parsed, fingerprint) = parse_file(PathBuf::from(&path), opt_delim, opt_enc).await?;
    let has_header = forced_header.unwrap_or_else(|| looks_like_header(&parsed.records));

    let mut guard = lock(&state)?;
    let id = guard.alloc_id();
    let mut doc = Document::from_parsed(id, Some(PathBuf::from(&path)), parsed, has_header);
    doc.set_fingerprint(fingerprint);
    let meta = doc.meta();
    guard.insert(doc);
    Ok(meta)
}

/// Parse the document's file with new delimiter/encoding/header overrides and
/// describe the outcome WITHOUT touching the open document. The apply step is
/// separate ([`apply_reparse`]) and guarded by the revision echoed back here.
#[tauri::command]
pub async fn preview_reparse(
    doc_id: u64,
    options: OpenOptions,
    max_rows: usize,
    state: Db<'_>,
) -> AppResult<ReparsePreview> {
    let (path, current) = read_doc(&state, doc_id, |doc| {
        let path = doc
            .path
            .clone()
            .ok_or_else(|| AppError::invalid("document has no file to reopen"))?;
        Ok((path, CurrentInterpretation::of(doc)))
    })?;

    let opt_delim = options.delimiter.as_deref().map(util::delimiter_to_byte);
    let opt_enc = options.encoding.as_deref().map(encoding::from_name);
    let (parsed, _) = parse_file(path, opt_delim, opt_enc).await?;
    let has_header = options.has_header_row.unwrap_or(current.has_header_row);

    Ok(reopen::build_preview(
        parsed, has_header, &current, max_rows,
    ))
}

/// Re-read the document's file with new settings, replacing the open document.
/// Rejected when the document changed since `expected_revision` was captured
/// (so unsaved edits can never be discarded without fresh confirmation); a
/// parse failure leaves the current document untouched.
#[tauri::command]
pub async fn apply_reparse(
    doc_id: u64,
    options: OpenOptions,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let (path, current_header) = read_doc(&state, doc_id, |doc| {
        doc.check_revision(expected_revision)?;
        let path = doc
            .path
            .clone()
            .ok_or_else(|| AppError::invalid("document has no file to reopen"))?;
        Ok((path, doc.has_header_row()))
    })?;

    let opt_delim = options.delimiter.as_deref().map(util::delimiter_to_byte);
    let opt_enc = options.encoding.as_deref().map(encoding::from_name);
    let (parsed, fingerprint) = parse_file(path.clone(), opt_delim, opt_enc).await?;
    let has_header = options.has_header_row.unwrap_or(current_header);

    write_doc(&state, doc_id, |doc| {
        // Re-check under the write lock: an edit may have landed while the
        // file was being parsed.
        doc.check_revision(expected_revision)?;
        let mut fresh = Document::from_parsed(doc_id, Some(path), parsed, has_header);
        // Continue the revision sequence so anything captured against the old
        // incarnation can never accidentally match the new one.
        fresh.set_revision(doc.revision() + 1);
        fresh.set_fingerprint(fingerprint);
        let meta = fresh.meta();
        *doc = fresh;
        Ok(meta)
    })
}

/// The stored fingerprint of the document's backing file, if any.
#[tauri::command]
pub fn get_file_fingerprint(doc_id: u64, state: Db<'_>) -> AppResult<Option<FileFingerprint>> {
    read_doc(&state, doc_id, |doc| Ok(doc.fingerprint()))
}

/// Compare the stored source fingerprint with the file on disk to detect
/// modifications made outside CEESVEE. Never mutates anything.
#[tauri::command]
pub async fn check_external_change(doc_id: u64, state: Db<'_>) -> AppResult<ExternalChange> {
    let (path, stored) = read_doc(&state, doc_id, |doc| {
        Ok((doc.path.clone(), doc.fingerprint()))
    })?;

    let Some(path) = path else {
        // Unsaved documents have no backing file to drift from.
        return Ok(ExternalChange {
            changed: false,
            exists: false,
            disk: None,
            stored,
        });
    };

    let disk = tauri::async_runtime::spawn_blocking(move || util::stat_fingerprint(&path))
        .await
        .map_err(|e| AppError::Other(format!("background task failed: {e}")))?;

    Ok(ExternalChange {
        // Only meaningful when we have a baseline: a missing stored
        // fingerprint (legacy sessions) never reports a change.
        changed: stored.is_some() && disk != stored,
        exists: disk.is_some(),
        disk,
        stored,
    })
}

#[tauri::command]
pub fn new_document(
    rows: Option<usize>,
    cols: Option<usize>,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let mut guard = lock(&state)?;
    let id = guard.alloc_id();
    let doc = Document::new_empty(id, cols.unwrap_or(4), rows.unwrap_or(50));
    let meta = doc.meta();
    guard.insert(doc);
    Ok(meta)
}

#[tauri::command]
pub fn close_document(
    doc_id: u64,
    state: Db<'_>,
    diagnostics_cache: State<'_, DiagnosticsCache>,
    profile_cache: State<'_, ProfileCache>,
    dedup_cache: State<'_, DedupCache>,
    compare_cache: State<'_, CompareCache>,
) -> AppResult<()> {
    lock(&state)?.remove(doc_id);
    diagnostics_cache.remove(doc_id);
    profile_cache.remove_doc(doc_id);
    dedup_cache.remove(doc_id);
    compare_cache.remove_doc(doc_id);
    Ok(())
}

#[tauri::command]
pub fn get_meta(doc_id: u64, state: Db<'_>) -> AppResult<DocumentMeta> {
    read_doc(&state, doc_id, |doc| Ok(doc.meta()))
}

#[tauri::command]
pub fn list_encodings() -> Vec<String> {
    encoding::SUPPORTED.iter().map(|s| s.to_string()).collect()
}

/// Drain and return any files passed at launch (e.g. via "Open with CEESVEE").
#[tauri::command]
pub fn take_pending_files(pending: State<'_, PendingFiles>) -> Vec<String> {
    pending
        .0
        .lock()
        .map(|mut files| std::mem::take(&mut *files))
        .unwrap_or_default()
}

// ----- jobs ----------------------------------------------------------------

/// Request cooperative cancellation of a running background job. Returns
/// whether a job with that id was still running.
#[tauri::command]
pub fn cancel_job(job_id: u64, jobs: State<'_, JobRegistry>) -> bool {
    jobs.cancel(job_id)
}

// ----- diagnostics ---------------------------------------------------------

/// The last completed diagnostics report for a document, if any. The report
/// carries the revision it was computed against; the UI offers a rescan when
/// the document has moved on.
#[tauri::command]
pub fn get_diagnostics(
    doc_id: u64,
    diagnostics_cache: State<'_, DiagnosticsCache>,
) -> Option<DiagnosticsReport> {
    diagnostics_cache.get(doc_id)
}

/// Start a background diagnostics scan, returning its job id immediately.
/// Progress streams over `job-progress`; on `job-finished` (done) the report
/// is available via `get_diagnostics`. Rejected up front when
/// `expected_revision` no longer matches the document.
#[tauri::command]
pub async fn start_diagnostics_scan(
    doc_id: u64,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
    diagnostics_cache: State<'_, DiagnosticsCache>,
) -> AppResult<u64> {
    let handle = doc_handle(&state, doc_id)?;
    // Fail fast (before spawning a job) when the caller's snapshot is stale.
    {
        let doc = handle.read().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
    }

    let ctx = jobs.begin_for_app(&app, "diagnostics", Some(doc_id));
    let job_id = ctx.id;
    let sink = diagnostics_cache.share();
    tauri::async_runtime::spawn(async move {
        // Terminal status (done / cancelled / failed) is emitted by
        // run_blocking; the report only becomes visible on success.
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let doc = handle.read().map_err(poisoned)?;
            // The document may have changed between the fast check above and
            // the read lock being granted; a stale scan would waste work and
            // its result would have to be discarded anyway.
            doc.check_revision(expected_revision)?;
            let report = diagnostics::scan(&doc, ctx)?;
            if let Ok(mut map) = sink.lock() {
                map.insert(doc_id, report);
            }
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

/// Replace the document's filter view with the rows affected by a
/// row-filterable diagnostic issue.
#[tauri::command]
pub fn apply_diagnostic_filter(
    doc_id: u64,
    issue_id: String,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        doc.check_revision(expected_revision)?;
        let rows = diagnostics::issue_rows(doc, &issue_id)?;
        doc.set_filter(rows);
        Ok(doc.meta())
    })
}

// ----- settings / profiles (F08) -------------------------------------------

fn settings_dir(app: &tauri::AppHandle) -> AppResult<std::path::PathBuf> {
    use tauri::Manager;
    app.path()
        .app_data_dir()
        .map_err(|e| AppError::Other(format!("application-data directory unavailable: {e}")))
}

/// Load persisted profiles + preferences (defaults when missing; a corrupt
/// file is preserved as a backup and defaults returned).
#[tauri::command]
pub fn get_settings(app: tauri::AppHandle) -> AppResult<AppSettings> {
    Ok(settings::load_settings(&settings_dir(&app)?))
}

/// Persist profiles + preferences atomically.
#[tauri::command]
pub fn set_settings(settings: AppSettings, app: tauri::AppHandle) -> AppResult<()> {
    settings::save_settings(&settings_dir(&app)?, &settings)
}

/// Check a document against a profile's column and data rules. Read-only.
#[tauri::command]
pub fn validate_profile(
    doc_id: u64,
    profile: FileProfile,
    state: Db<'_>,
) -> AppResult<ProfileValidation> {
    read_doc(&state, doc_id, |doc| {
        settings::validate_profile(doc, &profile)
    })
}

// ----- CSV compare (F09) --------------------------------------------------------

/// Start a comparison of two open documents; returns the job id, which also
/// identifies the stored result. Strictly read-only for both documents.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn start_compare(
    left_doc_id: u64,
    right_doc_id: u64,
    spec: CompareSpec,
    expected_left_revision: u64,
    expected_right_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
    compare_cache: State<'_, CompareCache>,
) -> AppResult<u64> {
    if left_doc_id == right_doc_id {
        return Err(AppError::invalid("pick two different documents to compare"));
    }
    let left_handle = doc_handle(&state, left_doc_id)?;
    let right_handle = doc_handle(&state, right_doc_id)?;
    {
        let (left, right) =
            compare::read_both(&left_handle, &right_handle, left_doc_id, right_doc_id)?;
        left.check_revision(expected_left_revision)?;
        right.check_revision(expected_right_revision)?;
    }

    let ctx = jobs.begin_for_app(&app, "compare", Some(left_doc_id));
    let job_id = ctx.id;
    let sink = compare_cache.share();
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let result = {
                let (left, right) =
                    compare::read_both(&left_handle, &right_handle, left_doc_id, right_doc_id)?;
                left.check_revision(expected_left_revision)?;
                right.check_revision(expected_right_revision)?;
                compare::compare(&left, &right, &spec, ctx)?
                // Both read guards drop here, BEFORE the cache lock (keeps
                // the cache -> documents lock order globally consistent).
            };
            if let Ok(mut map) = sink.lock() {
                map.insert(job_id, result);
            }
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

/// Summary + identity of a stored comparison.
#[tauri::command]
pub fn get_compare_info(
    compare_id: u64,
    compare_cache: State<'_, CompareCache>,
) -> Option<CompareInfo> {
    compare_cache.with(compare_id, |result| result.info(compare_id))
}

/// One page of hydrated results (keys + cell differences), optionally
/// filtered by status. Rejected once either document moved past the compared
/// revision — stale results are never served.
#[tauri::command]
pub fn get_compare_results(
    compare_id: u64,
    offset: usize,
    count: usize,
    statuses: Option<Vec<String>>,
    state: Db<'_>,
    compare_cache: State<'_, CompareCache>,
) -> AppResult<ComparePage> {
    let filter: Option<Vec<DiffStatus>> = statuses
        .map(|names| {
            names
                .iter()
                .map(|n| {
                    DiffStatus::parse(n)
                        .ok_or_else(|| AppError::invalid(format!("unknown status: {n}")))
                })
                .collect::<AppResult<Vec<_>>>()
        })
        .transpose()?;

    compare_cache
        .with(compare_id, |result| -> AppResult<ComparePage> {
            let left_handle = doc_handle(&state, result.left_doc)?;
            let right_handle = doc_handle(&state, result.right_doc)?;
            let (left, right) = compare::read_both(
                &left_handle,
                &right_handle,
                result.left_doc,
                result.right_doc,
            )?;
            let (records, total_filtered) =
                compare::results_page(result, &left, &right, offset, count, filter.as_deref())?;
            Ok(ComparePage {
                records,
                total_filtered,
            })
        })
        .ok_or_else(|| AppError::invalid("comparison no longer exists"))?
}

/// Export the added / removed / changed rows of a comparison to a file using
/// the atomic streaming pipeline, or a structured JSON change report.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn start_compare_export(
    compare_id: u64,
    which: String,
    path: String,
    options: ExportOptions,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
    compare_cache: State<'_, CompareCache>,
) -> AppResult<u64> {
    let ctx = jobs.begin_for_app(&app, "export", None);
    let job_id = ctx.id;
    let sink = compare_cache.share();
    let state_docs = lock(&state)?;
    // Resolve document handles up front (the job outlives this command).
    let (left_doc, right_doc) = compare_cache
        .with(compare_id, |r| (r.left_doc, r.right_doc))
        .ok_or_else(|| AppError::invalid("comparison no longer exists"))?;
    let left_handle = state_docs.doc(left_doc)?;
    let right_handle = state_docs.doc(right_doc)?;
    drop(state_docs);

    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let map = sink
                .lock()
                .map_err(|_| AppError::Other("internal compare lock error".into()))?;
            let result = map
                .get(&compare_id)
                .ok_or_else(|| AppError::invalid("comparison no longer exists"))?;
            let (left, right) =
                compare::read_both(&left_handle, &right_handle, left_doc, right_doc)?;
            left.check_revision(result.left_revision)?;
            right.check_revision(result.right_revision)?;

            let dest = std::path::PathBuf::from(&path);
            match which.as_str() {
                "report" => {
                    // Stream the full hydrated record list as a JSON array.
                    save_mod::atomic_write(&dest, options.backup, |file| {
                        use std::io::Write;
                        let mut written: u64 = 0;
                        let mut w = |bytes: &[u8], file: &mut std::fs::File| -> AppResult<()> {
                            file.write_all(bytes)?;
                            written += bytes.len() as u64;
                            Ok(())
                        };
                        w(b"[", file)?;
                        const PAGE: usize = 2048;
                        let mut offset = 0;
                        let mut first = true;
                        loop {
                            ctx.check()?;
                            let (records, _) =
                                compare::results_page(result, &left, &right, offset, PAGE, None)?;
                            if records.is_empty() {
                                break;
                            }
                            for record in &records {
                                if !first {
                                    w(b",\n", file)?;
                                }
                                first = false;
                                let json = serde_json::to_vec(record).map_err(|e| {
                                    AppError::Other(format!("report serialization failed: {e}"))
                                })?;
                                w(&json, file)?;
                            }
                            offset += PAGE;
                        }
                        w(b"]\n", file)?;
                        Ok(written)
                    })?;
                }
                _ => {
                    let status = DiffStatus::parse(&which)
                        .ok_or_else(|| AppError::invalid("unknown export selection"))?;
                    let rows = compare::rows_for_status(result, status);
                    ctx.set_total(rows.len() as u64);
                    // Added rows live in the RIGHT document; everything else
                    // exports from the left.
                    let doc: &Document = if status == DiffStatus::Added {
                        &right
                    } else {
                        &left
                    };
                    let cols: Vec<usize> = (0..doc.n_cols()).collect();
                    save_mod::atomic_write(&dest, options.backup, |file| {
                        export::write_view(doc, &rows, &cols, &options, file, Some(ctx))
                    })?;
                }
            }
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

// ----- duplicate finder (F07) --------------------------------------------------

/// The last completed duplicate report for a document, if any (the report
/// carries the revision it was computed against; the UI offers a rescan when
/// the document moves on).
#[tauri::command]
pub fn get_duplicate_report(
    doc_id: u64,
    dedup_cache: State<'_, DedupCache>,
) -> Option<DuplicateReport> {
    dedup_cache.get(doc_id)
}

/// Start a background duplicate scan; returns the job id. On completion the
/// report is cached and available via `get_duplicate_report`.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn start_duplicate_scan(
    doc_id: u64,
    spec: DedupSpec,
    scope: ExportScope,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
    dedup_cache: State<'_, DedupCache>,
) -> AppResult<u64> {
    let handle = doc_handle(&state, doc_id)?;
    {
        let doc = handle.read().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
    }
    let ctx = jobs.begin_for_app(&app, "dedup", Some(doc_id));
    let job_id = ctx.id;
    let sink = dedup_cache.share();
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let doc = handle.read().map_err(poisoned)?;
            doc.check_revision(expected_revision)?;
            let report = dedup::find_duplicates(&doc, &spec, &scope, Some(ctx))?;
            if let Ok(mut map) = sink.lock() {
                map.insert(doc_id, report);
            }
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

/// Filter the grid to every row belonging to a duplicate group.
#[tauri::command]
pub async fn apply_duplicate_filter(
    doc_id: u64,
    spec: DedupSpec,
    scope: ExportScope,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        // Compute against a consistent read snapshot first…
        let rows = {
            let doc = handle.read().map_err(poisoned)?;
            doc.check_revision(expected_revision)?;
            dedup::duplicate_row_indices(&doc, &spec, &scope)?
        };
        // …then swap the filter view in (re-checked: an edit may have raced).
        let mut doc = handle.write().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
        doc.set_filter(rows);
        Ok(doc.meta())
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Remove the non-kept rows of every duplicate group as ONE undoable
/// operation, guarded by the report's revision. Runs as a job (the scan is
/// cancellable; the commit itself is brief).
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn apply_deduplicate(
    doc_id: u64,
    spec: DedupSpec,
    scope: ExportScope,
    keep_strategy: DuplicateKeepStrategy,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
) -> AppResult<u64> {
    let handle = doc_handle(&state, doc_id)?;
    {
        let doc = handle.read().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
    }
    let ctx = jobs.begin_for_app(&app, "dedup", Some(doc_id));
    let job_id = ctx.id;
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let removals = {
                let doc = handle.read().map_err(poisoned)?;
                doc.check_revision(expected_revision)?;
                dedup::removal_rows(&doc, &spec, &scope, keep_strategy, Some(ctx))?
            };
            ctx.check()?; // last cancellation point before the commit
            let mut doc = handle.write().map_err(poisoned)?;
            // Results cannot be applied after the document revision changes.
            doc.check_revision(expected_revision)?;
            doc.clear_filter();
            doc.delete_rows(removals)?;
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

// ----- data-cleaning transformations (F06) -----------------------------------

/// Compute a transform's full effect WITHOUT mutating the document: affected
/// counts, before/after examples, parse failures, and column changes. Bad
/// parameters (invalid regex or date format) fail here, before any scan.
#[tauri::command]
pub async fn preview_transform(
    doc_id: u64,
    spec: TransformSpec,
    scope: ExportScope,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<TransformPreview> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let doc = handle.read().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
        Ok(transform::compute(&doc, &spec, &scope, None)?.preview)
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Apply a previewed transform as ONE undoable operation, guarded by the
/// preview's revision. Runs as a job: the change list is computed under the
/// read lock (cancellable, with progress), then committed under a brief
/// write lock. `failAll` refuses to commit when any cell cannot convert.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn apply_transform(
    doc_id: u64,
    spec: TransformSpec,
    scope: ExportScope,
    policy: TransformErrorPolicy,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
) -> AppResult<u64> {
    let handle = doc_handle(&state, doc_id)?;
    {
        let doc = handle.read().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
    }

    let ctx = jobs.begin_for_app(&app, "transform", Some(doc_id));
    let job_id = ctx.id;
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let computed = {
                let doc = handle.read().map_err(poisoned)?;
                doc.check_revision(expected_revision)?;
                transform::compute(&doc, &spec, &scope, Some(ctx))?
                // Read lock released here; commit re-validates below.
            };
            if policy == TransformErrorPolicy::FailAll && computed.preview.parse_failures > 0 {
                return Err(AppError::invalid(format!(
                    "{} cell(s) cannot be converted — fix them or apply with \"skip invalid\"",
                    computed.preview.parse_failures
                )));
            }
            ctx.check()?; // last cancellation point before the commit

            let mut doc = handle.write().map_err(poisoned)?;
            // An edit may have raced between the locks; never commit against
            // data the preview didn't see.
            doc.check_revision(expected_revision)?;
            // Cell values (and possibly column structure) change: the filter
            // view may no longer be correct, so drop it. The front end
            // re-applies its filter spec afterwards.
            doc.clear_filter();
            transform::commit(&mut doc, computed.changes)?;
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

// ----- column profiling (F05) -----------------------------------------------

/// A still-valid cached profile for (column, scope), if one exists. Validity
/// is per column: edits to other columns don't evict it.
#[tauri::command]
pub fn get_column_profile(
    doc_id: u64,
    column: usize,
    scope: ProfileScope,
    state: Db<'_>,
    profile_cache: State<'_, ProfileCache>,
) -> AppResult<Option<ColumnProfile>> {
    let handle = doc_handle(&state, doc_id)?;
    let doc = handle.read().map_err(poisoned)?;
    Ok(profile_cache.get_valid(&doc, column, scope))
}

/// Start a background profile scan for one column; returns the job id.
/// Progress/cancellation via the shared job events; on completion the result
/// is cached and available through `get_column_profile`.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn start_column_profile(
    doc_id: u64,
    column: usize,
    scope: ProfileScope,
    options: Option<ProfileOptions>,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
    profile_cache: State<'_, ProfileCache>,
) -> AppResult<u64> {
    let handle = doc_handle(&state, doc_id)?;
    {
        let doc = handle.read().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
        if column >= doc.n_cols() {
            return Err(AppError::invalid("column out of range"));
        }
    }

    let options = options.unwrap_or_default();
    let ctx = jobs.begin_for_app(&app, "profile", Some(doc_id));
    let job_id = ctx.id;
    let sink = profile_cache.share();
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let doc = handle.read().map_err(poisoned)?;
            doc.check_revision(expected_revision)?;
            let profile = profile::profile_column(&doc, column, scope, &options, ctx)?;
            if let Ok(mut map) = sink.lock() {
                map.insert((doc_id, column, scope), profile);
            }
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

// ----- windowed reads ----------------------------------------------------

#[tauri::command]
pub fn get_rows(doc_id: u64, start: usize, count: usize, state: Db<'_>) -> AppResult<RowsResponse> {
    read_doc(&state, doc_id, |doc| Ok(doc.get_rows(start, count)))
}

#[tauri::command]
pub fn selection_stats(doc_id: u64, rect: CellRect, state: Db<'_>) -> AppResult<SelectionStats> {
    read_doc(&state, doc_id, |doc| Ok(doc.selection_stats(rect)))
}

#[tauri::command]
pub fn column_summaries(doc_id: u64, state: Db<'_>) -> AppResult<Vec<ColumnSummary>> {
    read_doc(&state, doc_id, |doc| Ok(doc.column_summaries()))
}

// ----- cell editing ------------------------------------------------------

#[tauri::command]
pub fn set_cell(
    doc_id: u64,
    row: usize,
    col: usize,
    value: String,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        let abs = abs_row(doc, row)?;
        doc.set_cell(abs, col, value)?;
        Ok(doc.meta())
    })
}

#[tauri::command]
pub fn set_cells(
    doc_id: u64,
    changes: Vec<(usize, usize, String)>,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        let mut translated = Vec::with_capacity(changes.len());
        for (row, col, value) in changes {
            translated.push((abs_row(doc, row)?, col, value));
        }
        doc.set_cells(translated)?;
        Ok(doc.meta())
    })
}

#[tauri::command]
pub fn paste(
    doc_id: u64,
    anchor_row: usize,
    anchor_col: usize,
    block: Vec<Vec<String>>,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        // Pasting can grow/reshape the grid, so it drops any active filter and
        // operates on the absolute anchor position.
        let abs = abs_insert_row(doc, anchor_row)?;
        doc.clear_filter();
        doc.paste(abs, anchor_col, block)?;
        Ok(doc.meta())
    })
}

// ----- row operations ----------------------------------------------------

#[tauri::command]
pub fn insert_rows(doc_id: u64, at: usize, count: usize, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        let abs = abs_insert_row(doc, at)?;
        doc.clear_filter();
        doc.insert_rows(abs, count)?;
        Ok(doc.meta())
    })
}

#[tauri::command]
pub fn delete_rows(doc_id: u64, indices: Vec<usize>, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        let mut abs = Vec::with_capacity(indices.len());
        for d in indices {
            abs.push(abs_row(doc, d)?);
        }
        doc.clear_filter();
        doc.delete_rows(abs)?;
        Ok(doc.meta())
    })
}

#[tauri::command]
pub fn move_row(doc_id: u64, from: usize, to: usize, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        let from_abs = abs_row(doc, from)?;
        let to_abs = abs_row(doc, to)?;
        doc.clear_filter();
        doc.move_row(from_abs, to_abs)?;
        Ok(doc.meta())
    })
}

// ----- column operations -------------------------------------------------

#[tauri::command]
pub fn insert_column(
    doc_id: u64,
    at: usize,
    name: String,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        // Column structure shifts the indices a filter references, so drop it.
        doc.clear_filter();
        doc.insert_column(at, name)?;
        Ok(doc.meta())
    })
}

#[tauri::command]
pub fn delete_columns(doc_id: u64, indices: Vec<usize>, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        doc.clear_filter();
        doc.delete_columns(indices)?;
        Ok(doc.meta())
    })
}

#[tauri::command]
pub fn rename_column(
    doc_id: u64,
    col: usize,
    name: String,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        doc.rename_column(col, name)?;
        Ok(doc.meta())
    })
}

#[tauri::command]
pub fn move_column(doc_id: u64, from: usize, to: usize, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        doc.clear_filter();
        doc.move_column(from, to)?;
        Ok(doc.meta())
    })
}

// ----- analysis ----------------------------------------------------------

#[tauri::command]
pub fn sort(doc_id: u64, keys: Vec<SortKey>, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        // Sorting reorders all rows, invalidating a filter view; drop it.
        doc.clear_filter();
        doc.sort(&keys)?;
        Ok(doc.meta())
    })
}

#[tauri::command]
pub fn set_header_mode(doc_id: u64, has_header: bool, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        // Re-interpreting the header row shifts all row indices; drop any filter.
        doc.clear_filter();
        doc.set_header_mode(has_header);
        Ok(doc.meta())
    })
}

#[tauri::command]
pub fn find(doc_id: u64, options: FindOptions, state: Db<'_>) -> AppResult<Vec<FindMatch>> {
    read_doc(&state, doc_id, |doc| find_mod::find(doc, &options))
}

#[tauri::command]
pub fn replace_all(
    doc_id: u64,
    options: FindOptions,
    replacement: String,
    state: Db<'_>,
) -> AppResult<ReplaceResult> {
    write_doc(&state, doc_id, |doc| {
        let changes = find_mod::replace_all(doc, &options, &replacement)?;
        let replaced = changes.len();
        doc.set_cells(changes)?;
        Ok(ReplaceResult {
            replaced,
            meta: doc.meta(),
        })
    })
}

// ----- filtering ---------------------------------------------------------

#[tauri::command]
pub fn set_filter(doc_id: u64, spec: FilterGroup, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        let view = filter_mod::matching_rows(doc, &spec)?;
        // A filter that excludes nothing isn't an active filter — avoids reporting
        // "N of N rows · filtered" for a match-all or empty spec.
        if view.len() == doc.n_rows() {
            doc.clear_filter();
        } else {
            doc.set_filter(view);
        }
        Ok(doc.meta())
    })
}

#[tauri::command]
pub fn clear_filter(doc_id: u64, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        doc.clear_filter();
        Ok(doc.meta())
    })
}

// ----- history -----------------------------------------------------------

#[tauri::command]
pub fn undo(doc_id: u64, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        // Undo/redo may reinstate rows the filter view doesn't account for, so the
        // view is dropped to keep coordinates consistent.
        doc.clear_filter();
        doc.undo()?;
        Ok(doc.meta())
    })
}

#[tauri::command]
pub fn redo(doc_id: u64, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        doc.clear_filter();
        doc.redo()?;
        Ok(doc.meta())
    })
}

// ----- save / export -------------------------------------------------------

/// Scan for characters the target encoding cannot represent, so the UI can
/// block a lossy export up front (nothing is ever substituted silently).
/// `scope` limits the scan to what will actually be written (default: all).
#[tauri::command]
pub async fn check_encoding_compatibility(
    doc_id: u64,
    encoding: String,
    scope: Option<ExportScope>,
    state: Db<'_>,
) -> AppResult<EncodingCompatibility> {
    const SAMPLE_LIMIT: usize = 100;
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let doc = handle.read().map_err(poisoned)?;
        let target = encoding::from_name(&encoding);
        let resolved = export_scope::resolve_scope(&doc, &scope.unwrap_or(ExportScope::All))?;
        let mut affected = 0usize;
        let mut samples = Vec::new();
        let mut record = |row: Option<usize>, col: usize, value: &str| {
            affected += 1;
            if samples.len() < SAMPLE_LIMIT {
                samples.push(EncodingIncompatibility {
                    row,
                    col,
                    value: value.chars().take(80).collect(),
                });
            }
        };
        if doc.has_header_row() {
            let headers = doc.headers();
            for &col in &resolved.cols {
                if encoding::has_unmappable(&headers[col], target) {
                    record(None, col, &headers[col]);
                }
            }
        }
        let rows = doc.rows();
        for &r in &resolved.rows {
            for &c in &resolved.cols {
                if encoding::has_unmappable(&rows[r][c], target) {
                    record(Some(r), c, &rows[r][c]);
                }
            }
        }
        Ok(EncodingCompatibility {
            encoding: target.name().to_string(),
            compatible: affected == 0,
            affected_cells: affected,
            samples,
        })
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// The row/column counts a scoped export would write, for the export dialog.
#[tauri::command]
pub fn export_scope_counts(
    doc_id: u64,
    scope: ExportScope,
    state: Db<'_>,
) -> AppResult<ScopeCounts> {
    read_doc(&state, doc_id, |doc| {
        export_scope::scope_counts(doc, &scope)
    })
}

/// Start an atomic streaming save of the COMPLETE document (Ctrl+S always
/// writes everything). Returns the job id; progress (rows + bytes) streams
/// over the shared job events, and `get_meta` reflects the saved state once
/// the job finishes. Guarded by `expected_revision`.
#[tauri::command]
pub async fn start_save(
    doc_id: u64,
    path: String,
    options: ExportOptions,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
) -> AppResult<u64> {
    let handle = doc_handle(&state, doc_id)?;
    // Fail fast (before spawning a job) when the caller's snapshot is stale.
    {
        let doc = handle.read().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
    }

    let ctx = jobs.begin_for_app(&app, "save", Some(doc_id));
    let job_id = ctx.id;
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let dest = PathBuf::from(&path);
            {
                let doc = handle.read().map_err(poisoned)?;
                doc.check_revision(expected_revision)?;
                ctx.set_total(doc.n_rows() as u64);
                save_mod::atomic_write(&dest, options.backup, |file| {
                    export::write_document(&doc, &options, file, Some(ctx))
                })?;
                // Read lock is dropped here, before taking the write lock.
            }

            let fingerprint = util::stat_fingerprint(&dest);
            let mut doc = handle.write().map_err(poisoned)?;
            if doc.check_revision(expected_revision).is_ok() {
                // Nothing changed while streaming: the file matches the
                // document, so record the save point and new baseline.
                doc.mark_saved(Some(dest));
                doc.set_fingerprint(fingerprint);
            } else if doc.path.as_deref() == Some(dest.as_path()) {
                // An edit raced the save: the file holds the pre-edit
                // snapshot, so the document stays dirty — but the file on
                // disk is still ours, so refresh the external-change
                // baseline.
                doc.set_fingerprint(fingerprint);
            }
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

/// Start a scoped, optionally split, atomic streaming export (F04). Writes
/// the requested slice to one or more files (plus an optional manifest) and
/// never touches the document's save point, path, or fingerprint.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn start_export(
    doc_id: u64,
    path: String,
    options: ExportOptions,
    scope: ExportScope,
    split: SplitOptions,
    write_manifest: bool,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
) -> AppResult<u64> {
    let handle = doc_handle(&state, doc_id)?;
    {
        let doc = handle.read().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
        // Validate the scope up front so obvious mistakes fail the invoke
        // instead of a background job.
        export_scope::resolve_scope(&doc, &scope)?;
    }

    let ctx = jobs.begin_for_app(&app, "export", Some(doc_id));
    let job_id = ctx.id;
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let doc = handle.read().map_err(poisoned)?;
            doc.check_revision(expected_revision)?;
            export_scope::run_export(
                &doc,
                Path::new(&path),
                &options,
                &scope,
                &split,
                write_manifest,
                ctx,
            )?;
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}
