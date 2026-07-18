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

use crate::annotations::{
    self, AnnotationExportFormat, AnnotationPredicate, AnnotationRegistry, AnnotationsExport,
    AnnotationsView, RematchReport, RowMarkPatch, TagDef, TagToColumnPreview, TagToColumnTarget,
};
use crate::append::{self, AppendCache, AppendInput, AppendOptions, AppendPreview, AppendReport};
use crate::archive::{self, ArchiveCache, ZipEntryInfo};
use crate::clipboard::{self, CopyFormat};
use crate::cluster::{self, ClusterCache, ClusterReport, ClusterSpec};
use crate::compare::{self, CompareCache, CompareInfo, ComparePage, CompareSpec, DiffStatus};
use crate::crossval::{self, CrossRule, CrossValCache, CrossValReport};
use crate::dedup::{self, DedupCache, DedupSpec, DuplicateKeepStrategy, DuplicateReport};
use crate::diagnostics::{self, DiagnosticsCache, DiagnosticsReport};
use crate::dialect::{self, CsvDialectOptions, DialectPreview};
use crate::dictionary::{
    self, DictionaryField, DictionaryFormat, DictionaryImportOutcome, DictionaryView, MergeMatchBy,
    MergePlan, MergeResolution,
};
use crate::document::{ChangeSummary, Document};
use crate::dto::{
    BackupPolicy, CellRect, ColumnSummary, DocumentMeta, EncodingCompatibility,
    EncodingIncompatibility, ExportOptions, ExportScope, ExternalChange, FileFingerprint,
    FilterGroup, FindMatch, FindOptions, IndexedOpenStart, JsonExportOptions, OpenOptions,
    ReparsePreview, ReplaceResult, RowsResponse, ScopeCounts, SelectionStats, SortKey,
    SplitOptions,
};
use crate::error::{AppError, AppResult};
use crate::follow::{self, FollowRegistry};
use crate::groupby::{self, GroupByPreview, GroupBySpec};
use crate::job::JobRegistry;
use crate::joins::{self, JoinPreview, JoinSpec};
use crate::journal::{self, RecoverableSession};
use crate::json_import::{JsonImportOptions, JsonImportPreview, JsonImportPreviewCache};
use crate::outlier::{
    self, CachedOutlier, OutlierAction, OutlierActionPreview, OutlierCache, OutlierSpec,
};
use crate::parse::{parse, ParseSettings, ParsedFile};
use crate::paste::{self, PasteOptions, PastePreview};
use crate::pii::{self, CachedPii, PiiCache, PiiSpec, RedactionAction, RedactionPreview};
use crate::profile::{self, ColumnProfile, ProfileCache, ProfileOptions, ProfileScope};
use crate::recipe::{self, BatchOptions, BatchReport, RecipeCache};
use crate::reopen::{self, CurrentInterpretation};
use crate::repair::{self, RepairPreview, RepairSpec};
use crate::reshape::{self, ReshapePreview, ReshapeSpec};
use crate::row_identity::KeySpec;
use crate::sampling::{
    self, SampleDestination, SamplePlan, SamplePreview, SampleRequest, SampleStart,
};
use crate::schema::{ColumnSchema, DocumentSchema, SchemaIssue};
use crate::schema_ops::{
    self, CellEditValidation, ConvertPreview, InvalidSampleReport, SchemaImportOutcome, SchemaInfo,
    SchemaScanCache,
};
use crate::semantic::{
    self, SemanticAction, SemanticActionPreview, SemanticCache, SemanticReport, SemanticType,
};
use crate::settings::{self, AppSettings, FileProfile, ProfileValidation};
use crate::state::{AppState, PendingFiles, SharedDocument};
use crate::tabular::DocumentSource;
use crate::transform::{self, TransformErrorPolicy, TransformPreview, TransformSpec};
use crate::{
    encoding, export, export_scope, filter as filter_mod, find as find_mod, index, json_export,
    save as save_mod, util,
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
    records
        .first()
        .is_some_and(|first| util::looks_like_header(first))
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

/// Files smaller than this skip the open-time memory estimate entirely (the
/// materialised size of a <64 MiB file can never cross the decision line).
const ESTIMATE_GUARD_MIN: u64 = 64 * 1024 * 1024;

/// The app-cache directory that index caches live under.
fn index_cache_root(app: &tauri::AppHandle) -> AppResult<PathBuf> {
    use tauri::Manager;
    Ok(app
        .path()
        .app_cache_dir()
        .map_err(|e| AppError::Other(format!("no cache directory: {e}")))?
        .join("indexes"))
}

#[tauri::command]
pub async fn open_file(
    path: String,
    options: Option<OpenOptions>,
    app: tauri::AppHandle,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let options = options.unwrap_or_default();
    let opt_delim = options.delimiter.as_deref().map(util::delimiter_to_byte);
    let opt_enc = options.encoding.as_deref().map(encoding::from_name);
    let forced_header = options.has_header_row;

    // Guard rail: a file expected to blow the in-memory budget must not be
    // loaded eagerly. The UI probes first and shows the open-mode dialog;
    // "Open editable" comes back here with `force_in_memory`.
    if !options.force_in_memory {
        let probe_path = PathBuf::from(&path);
        let size = std::fs::metadata(&probe_path).map(|m| m.len()).unwrap_or(0);
        if size > ESTIMATE_GUARD_MIN {
            let est = tauri::async_runtime::spawn_blocking(move || index::estimate(&probe_path))
                .await
                .map_err(|e| AppError::Other(format!("background task failed: {e}")))??;
            if est.needs_decision {
                return Err(AppError::invalid(
                    "this file is large — choose how to open it (read-only indexed, or fully in memory)",
                ));
            }
        }
    }

    let (parsed, fingerprint) = parse_file(PathBuf::from(&path), opt_delim, opt_enc).await?;
    let has_header = forced_header.unwrap_or_else(|| looks_like_header(&parsed.records));

    let mut guard = lock(&state)?;
    let id = guard.alloc_id();
    let mut doc = Document::from_parsed(id, Some(PathBuf::from(&path)), parsed, has_header);
    doc.set_fingerprint(fingerprint);
    attach_journal_if_enabled(&app, &mut doc);
    let meta = doc.meta();
    guard.insert(doc);
    Ok(meta)
}

/// F16: attach a fresh crash-recovery journal when the opt-in is on and the
/// document has a source file. Best-effort — journaling never blocks opens.
fn attach_journal_if_enabled(app: &tauri::AppHandle, doc: &mut Document) {
    let Ok(dir) = settings_dir(app) else { return };
    let settings = settings::load_settings(&dir);
    if !settings.recovery_enabled {
        return;
    }
    let meta = doc.meta();
    if meta.path.is_none() {
        return;
    }
    let Ok(recovery) = recovery_dir(app) else {
        return;
    };
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = recovery.join(format!("{}-{epoch}.journal", meta.id));
    if let Ok(writer) = journal::JournalWriter::create(path, &doc.journal_header()) {
        doc.attach_journal(writer);
    }
}

fn recovery_dir(app: &tauri::AppHandle) -> AppResult<PathBuf> {
    use tauri::Manager;
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| AppError::Other(format!("no data directory: {e}")))?
        .join("recovery"))
}

/// Handles returned by `start_archive_extract` (F17).
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveExtractStart {
    pub job_id: u64,
    pub token: u64,
}

/// List the entries of a ZIP archive for the chooser dialog (F17).
#[tauri::command]
pub async fn list_archive_entries(path: String) -> AppResult<Vec<ZipEntryInfo>> {
    tauri::async_runtime::spawn_blocking(move || archive::list_zip_entries(Path::new(&path)))
        .await
        .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Extract a gzip member or a chosen ZIP entry into a guarded cache dir as a
/// cancellable job (progress = decompressed bytes). The result parks under
/// the returned token until `open_archive_document` consumes it or
/// `discard_archive` drops it. `allow_large` overrides the compression-ratio
/// guard after an explicit confirmation.
#[tauri::command]
pub async fn start_archive_extract(
    path: String,
    entry: Option<String>,
    allow_large: bool,
    app: tauri::AppHandle,
    jobs: State<'_, JobRegistry>,
    archives: State<'_, ArchiveCache>,
) -> AppResult<ArchiveExtractStart> {
    let cache_root = index_cache_root(&app)?;
    let token = archives.reserve();
    let sink = (*archives).clone();
    let ctx = jobs.begin_for_app(&app, "archiveExtract", None);
    let job_id = ctx.id;
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let pending = archive::extract_to_pending(
                Path::new(&path),
                entry.as_deref(),
                &cache_root,
                allow_large,
                &mut |delta| ctx.advance(delta),
            )?;
            sink.fulfill(token, pending);
            Ok(())
        })
        .await;
    });
    Ok(ArchiveExtractStart { job_id, token })
}

/// Estimate the in-memory cost of the extracted entry, so the UI can offer
/// indexed mode exactly like a plain-file open (F17).
#[tauri::command]
pub async fn pending_archive_estimate(
    token: u64,
    archives: State<'_, ArchiveCache>,
) -> AppResult<index::OpenEstimate> {
    let path = archives
        .data_path(token)
        .ok_or_else(|| AppError::invalid("the extracted file is no longer available"))?;
    tauri::async_runtime::spawn_blocking(move || index::estimate(&path))
        .await
        .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Open a parked extraction as a document (F17). `mode` is "editable"
/// (parse fully, temp released immediately) or "indexed" (background index
/// job; the temp stays alive with the document). Never edits the archive.
#[tauri::command]
pub async fn open_archive_document(
    token: u64,
    mode: String,
    options: Option<OpenOptions>,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
    archives: State<'_, ArchiveCache>,
) -> AppResult<IndexedOpenStart> {
    let pending = archives
        .take(token)
        .ok_or_else(|| AppError::invalid("the extracted file is no longer available"))?;
    let options = options.unwrap_or_default();
    let doc_id = lock(&state)?.alloc_id();

    match mode.as_str() {
        "editable" => {
            let opt_delim = options.delimiter.as_deref().map(util::delimiter_to_byte);
            let opt_enc = options.encoding.as_deref().map(encoding::from_name);
            let forced_header = options.has_header_row;
            let app_for_job = app.clone();
            tauri::async_runtime::spawn_blocking(move || {
                use tauri::Manager;
                let bytes = std::fs::read(&pending.data_path)?;
                let parsed = parse(
                    &bytes,
                    &ParseSettings {
                        delimiter: opt_delim,
                        encoding: opt_enc,
                    },
                )?;
                let has_header =
                    forced_header.unwrap_or_else(|| looks_like_header(&parsed.records));
                let mut doc = Document::from_parsed(doc_id, None, parsed, has_header);
                doc.set_archive_origin(pending.origin.clone(), None);
                let registry = app_for_job.state::<Mutex<AppState>>();
                registry
                    .lock()
                    .map_err(|_| AppError::Other("internal state lock error".into()))?
                    .insert(doc);
                // `pending.guard` drops here: the temp file is gone, the
                // rows live in memory.
                Ok(IndexedOpenStart { job_id: 0, doc_id })
            })
            .await
            .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
        }
        _ => {
            // Indexed: reuse the F10 job kind so the front end's existing
            // openIndexed completion path adds the tab.
            let cache_root = index_cache_root(&app)?;
            let ctx = jobs.begin_for_app(&app, "openIndexed", Some(doc_id));
            let job_id = ctx.id;
            let app_for_job = app.clone();
            tauri::async_runtime::spawn(async move {
                let _ = crate::job::run_blocking(ctx, move |ctx| {
                    use tauri::Manager;
                    let total = std::fs::metadata(&pending.data_path)
                        .map(|m| m.len())
                        .unwrap_or(0);
                    ctx.set_total(total);
                    let settings = index::IndexSettings {
                        delimiter: options.delimiter.as_deref().map(util::delimiter_to_byte),
                        encoding: options.encoding.as_deref().map(encoding::from_name),
                        has_header_row: options.has_header_row,
                        chunk_size: 0,
                    };
                    let indexed =
                        index::build_index(&pending.data_path, &cache_root, &settings, &mut |d| {
                            ctx.advance(d)
                        })?;
                    let mut doc = Document::from_index(doc_id, None, indexed);
                    // The index may read the extracted temp directly (UTF-8
                    // path): the guard moves into the document so the file
                    // outlives it.
                    doc.set_archive_origin(pending.origin.clone(), Some(pending.guard));
                    let registry = app_for_job.state::<Mutex<AppState>>();
                    registry
                        .lock()
                        .map_err(|_| AppError::Other("internal state lock error".into()))?
                        .insert(doc);
                    Ok(())
                })
                .await;
            });
            Ok(IndexedOpenStart { job_id, doc_id })
        }
    }
}

/// Drop a parked extraction and delete its cache directory (F17).
#[tauri::command]
pub fn discard_archive(token: u64, archives: State<'_, ArchiveCache>) {
    archives.discard(token);
}

/// Sample a file and estimate the in-memory cost of opening it editable, so
/// the UI can offer read-only (indexed) mode for huge files (F10).
#[tauri::command]
pub async fn probe_open(path: String) -> AppResult<index::OpenEstimate> {
    tauri::async_runtime::spawn_blocking(move || index::estimate(Path::new(&path)))
        .await
        .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Open a file in indexed read-only mode (F10): a background job scans the
/// file once (progress = bytes, cancellable), builds the record index, and
/// registers the document under the returned `doc_id` when it finishes. The
/// front end fetches the meta after the job's `job-finished` event.
#[tauri::command]
pub async fn start_open_indexed(
    path: String,
    options: Option<OpenOptions>,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
) -> AppResult<IndexedOpenStart> {
    let options = options.unwrap_or_default();
    let doc_id = lock(&state)?.alloc_id();
    let cache_root = index_cache_root(&app)?;

    let ctx = jobs.begin_for_app(&app, "openIndexed", Some(doc_id));
    let job_id = ctx.id;
    let app_for_job = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            use tauri::Manager;
            let source = PathBuf::from(&path);
            let total = std::fs::metadata(&source).map(|m| m.len()).unwrap_or(0);
            ctx.set_total(total);
            // Fingerprint from BEFORE the scan: if the file changes while
            // scanning, the external-change watcher flags it afterwards.
            let fingerprint = util::stat_fingerprint(&source);
            let settings = index::IndexSettings {
                delimiter: options.delimiter.as_deref().map(util::delimiter_to_byte),
                encoding: options.encoding.as_deref().map(encoding::from_name),
                has_header_row: options.has_header_row,
                chunk_size: 0,
            };
            let indexed = index::build_index(&source, &cache_root, &settings, &mut |delta| {
                ctx.advance(delta)
            })?;
            let mut doc = Document::from_index(doc_id, Some(source), indexed);
            doc.set_fingerprint(fingerprint);
            let registry = app_for_job.state::<Mutex<AppState>>();
            registry
                .lock()
                .map_err(|_| AppError::Other("internal state lock error".into()))?
                .insert(doc);
            Ok(())
        })
        .await;
    });
    Ok(IndexedOpenStart { job_id, doc_id })
}

/// Materialise an indexed document into a fully editable in-memory document
/// (F10 convert-to-editable). Re-runs the memory estimate first; pass `force`
/// to convert anyway. Runs as a job (progress = rows, cancellable).
#[tauri::command]
pub async fn start_convert_to_editable(
    doc_id: u64,
    force: bool,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
) -> AppResult<u64> {
    let handle = doc_handle(&state, doc_id)?;
    {
        let doc = handle.read().map_err(poisoned)?;
        if doc.is_editable() {
            return Err(AppError::invalid("document is already editable"));
        }
        if !force {
            if let Some(path) = doc.path.as_deref() {
                let est = index::estimate(path)?;
                if est.needs_decision {
                    return Err(AppError::invalid(format!(
                        "the estimated in-memory size is about {} MB, which may exhaust memory — \
                         export a slice instead, or convert anyway",
                        est.estimated_memory / (1024 * 1024)
                    )));
                }
            }
        }
    }

    let ctx = jobs.begin_for_app(&app, "convertEditable", Some(doc_id));
    let job_id = ctx.id;
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            // Stream the rows out under a read lock (reads stay available),
            // then commit under a brief write lock, revision-guarded.
            let (rows, revision) = {
                let doc = handle.read().map_err(poisoned)?;
                if doc.is_editable() {
                    return Err(AppError::invalid("document is already editable"));
                }
                let n = doc.n_rows();
                ctx.set_total(n as u64);
                let mut rows: Vec<Vec<String>> = Vec::with_capacity(n);
                let mut pending = 0u64;
                doc.visit_rows(0..n, &mut |_, row| {
                    rows.push(row.to_vec());
                    pending += 1;
                    if pending >= 4096 {
                        ctx.advance(pending)?;
                        pending = 0;
                    }
                    Ok(true)
                })?;
                ctx.advance(pending)?;
                (rows, doc.revision())
            };
            ctx.check()?; // last cancellation point before the commit

            let mut doc = handle.write().map_err(poisoned)?;
            // A concurrent reindex/filter would have bumped the revision; the
            // materialised rows would no longer match.
            doc.check_revision(revision)?;
            doc.make_editable(rows)?;
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

/// Rebuild an indexed document's record index from its file (the reload path
/// after an external change). Encoding and delimiter are re-detected; the
/// header choice is kept. Runs as a job (progress = bytes, cancellable).
#[tauri::command]
pub async fn start_reindex(
    doc_id: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
) -> AppResult<u64> {
    let handle = doc_handle(&state, doc_id)?;
    let (path, has_header) = {
        let doc = handle.read().map_err(poisoned)?;
        if doc.is_editable() {
            return Err(AppError::invalid(
                "only indexed documents reload by re-indexing",
            ));
        }
        let path = doc
            .path
            .clone()
            .ok_or_else(|| AppError::invalid("document has no file to reload"))?;
        (path, doc.has_header_row())
    };
    let cache_root = index_cache_root(&app)?;

    let ctx = jobs.begin_for_app(&app, "reindex", Some(doc_id));
    let job_id = ctx.id;
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let total = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            ctx.set_total(total);
            let fingerprint = util::stat_fingerprint(&path);
            let settings = index::IndexSettings {
                has_header_row: Some(has_header),
                ..Default::default()
            };
            let indexed = index::build_index(&path, &cache_root, &settings, &mut |delta| {
                ctx.advance(delta)
            })?;

            let mut doc = handle.write().map_err(poisoned)?;
            let mut fresh = Document::from_index(doc_id, Some(path.clone()), indexed);
            // Continue the revision sequence so anything captured against the
            // old incarnation can never accidentally match the new one.
            fresh.set_revision(doc.revision() + 1);
            fresh.set_fingerprint(fingerprint);
            // Schema entries key on stable IDs, which restart positionally on
            // a reload — they re-attach to the same columns (F31). The data
            // dictionary (F38) carries across on the same principle.
            fresh.inherit_schema(&doc);
            fresh.inherit_dictionary(&doc);
            *doc = fresh;
            Ok(())
        })
        .await;
    });
    Ok(job_id)
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
    app: tauri::AppHandle,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let (path, current_header) = read_doc(&state, doc_id, |doc| {
        // Indexed documents reload through `start_reindex` (streaming), never
        // by materialising the whole file here.
        doc.ensure_editable()?;
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
        // A journal against the OLD interpretation must not survive (its
        // replay would target coordinates that no longer exist), and merely
        // dropping the writer would leave the file to be offered as a
        // recovery on the next startup.
        if let Some(journal) = doc.take_journal() {
            journal.delete();
        }
        let mut fresh = Document::from_parsed(doc_id, Some(path), parsed, has_header);
        // Continue the revision sequence so anything captured against the old
        // incarnation can never accidentally match the new one.
        fresh.set_revision(doc.revision() + 1);
        fresh.set_fingerprint(fingerprint);
        // Schema entries key on stable IDs, which restart positionally on a
        // reparse — they re-attach to the same columns (F31). The data
        // dictionary (F38) carries across on the same principle.
        fresh.inherit_schema(doc);
        fresh.inherit_dictionary(doc);
        // F40 annotations are NOT held on the document: they live in the
        // doc_id-keyed AnnotationRegistry, so this whole-document swap preserves
        // them automatically. They re-resolve against the fresh content on the
        // next read; the front end calls `annotations_rematch` after a reparse
        // to surface any newly-ambiguous / orphaned annotations for review.
        // Journaling continues against the NEW interpretation.
        attach_journal_if_enabled(&app, &mut fresh);
        let meta = fresh.meta();
        *doc = fresh;
        Ok(meta)
    })
}

// ----- follow / tail mode (F19) -----------------------------------------------

/// Open a plain, uncompressed file in READ-ONLY follow mode (F19): a full
/// parse, then a watcher that appends complete records as the file grows.
#[tauri::command]
pub async fn start_follow(
    path: String,
    app: tauri::AppHandle,
    state: Db<'_>,
    follows: State<'_, FollowRegistry>,
) -> AppResult<DocumentMeta> {
    let source = PathBuf::from(&path);
    if !source.is_file() {
        return Err(AppError::invalid("not a file"));
    }
    let start_metadata = std::fs::metadata(&source)?;
    let start_offset = start_metadata.len();
    let identity = follow::FileIdentity::of(&start_metadata);
    let (parsed, fingerprint) = parse_file(source.clone(), None, None).await?;
    let delimiter = parsed.delimiter;
    let has_header = looks_like_header(&parsed.records);

    let mut guard = lock(&state)?;
    let id = guard.alloc_id();
    let mut doc = Document::from_parsed(id, Some(source.clone()), parsed, has_header);
    // Appended records are validated against the OPENED encoding; only the
    // UTF-8 family can be checked byte-exactly (ASCII appends to a legacy
    // single-byte file pass the NUL catch-all instead).
    let require_utf8 = doc.encoding_name().eq_ignore_ascii_case("UTF-8");
    doc.set_fingerprint(fingerprint);
    doc.set_follow(true);
    let n_cols = doc.n_cols();
    let meta = doc.meta();
    guard.insert(doc);
    let handle = guard.doc(id)?;
    drop(guard);

    let control = follow::spawn_watcher(
        app,
        handle,
        follow::WatcherConfig {
            doc_id: id,
            path: source,
            start_offset,
            delimiter,
            n_cols,
            identity,
            require_utf8,
        },
    );
    follows.insert(id, control);
    Ok(meta)
}

/// Pause or resume a follow watcher. Pausing stops VIEW updates only —
/// the file keeps its bytes and polling resumes from the same offset.
#[tauri::command]
pub fn set_follow_paused(
    doc_id: u64,
    paused: bool,
    follows: State<'_, FollowRegistry>,
) -> AppResult<()> {
    if follows.set_paused(doc_id, paused) {
        Ok(())
    } else {
        Err(AppError::invalid("that document is not being followed"))
    }
}

/// Filter the grid to rows from `from_row` onward (F19: "only newly added
/// rows"). The range is LIVE — records appended by the watcher extend it —
/// and clearing uses the ordinary clear_filter.
#[tauri::command]
pub fn set_row_range_filter(
    doc_id: u64,
    from_row: usize,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        let rows: Vec<usize> = (from_row.min(doc.n_rows())..doc.n_rows()).collect();
        doc.set_follow_range(Some(from_row));
        doc.set_filter(rows)?;
        Ok(doc.meta())
    })
}

/// Stop following: the watcher exits, its file handle closes, and the
/// document stays open as a read-only snapshot.
#[tauri::command]
pub fn stop_follow(
    doc_id: u64,
    state: Db<'_>,
    follows: State<'_, FollowRegistry>,
) -> AppResult<()> {
    follows.stop(doc_id);
    if let Ok(handle) = doc_handle(&state, doc_id) {
        if let Ok(mut doc) = handle.write() {
            doc.set_follow(false);
        }
    }
    Ok(())
}

// ----- advanced dialect / preamble import (F18) -----------------------------------

/// Parse the document's file under a full dialect (preamble skips, comment
/// prefix, custom quoting/escaping, multi-row headers, null tokens) and
/// describe the outcome WITHOUT touching the open document.
#[tauri::command]
pub async fn preview_dialect(
    doc_id: u64,
    dialect: CsvDialectOptions,
    state: Db<'_>,
) -> AppResult<DialectPreview> {
    let path = read_doc(&state, doc_id, |doc| {
        doc.path
            .clone()
            .ok_or_else(|| AppError::invalid("document has no file to reinterpret"))
    })?;
    tauri::async_runtime::spawn_blocking(move || {
        let bytes = std::fs::read(&path)?;
        dialect::preview(&bytes, &dialect)
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Re-read the document's file under the previewed dialect, replacing the
/// open document — the same guarded reparse workflow as F02: rejected when
/// the document moved past `expected_revision`, so unsaved edits are never
/// silently discarded, and a parse failure leaves everything untouched.
/// Saving afterwards writes only the current grid — skipped preamble and
/// comment records are never re-added.
#[tauri::command]
pub async fn apply_dialect(
    doc_id: u64,
    dialect: CsvDialectOptions,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let path = read_doc(&state, doc_id, |doc| {
        doc.ensure_editable()?;
        doc.check_revision(expected_revision)?;
        doc.path
            .clone()
            .ok_or_else(|| AppError::invalid("document has no file to reinterpret"))
    })?;

    let path_for_parse = path.clone();
    let (parsed_file, has_headers) =
        tauri::async_runtime::spawn_blocking(move || -> AppResult<_> {
            let bytes = std::fs::read(&path_for_parse)?;
            let parsed = dialect::parse_with_dialect(&bytes, &dialect)?;
            Ok(dialect::into_parsed_file(parsed, &dialect))
        })
        .await
        .map_err(|e| AppError::Other(format!("background task failed: {e}")))??;
    let fingerprint = util::stat_fingerprint(&path);

    write_doc(&state, doc_id, |doc| {
        doc.check_revision(expected_revision)?;
        let mut fresh = Document::from_parsed(doc_id, Some(path), parsed_file, has_headers);
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

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub fn close_document(
    doc_id: u64,
    state: Db<'_>,
    diagnostics_cache: State<'_, DiagnosticsCache>,
    profile_cache: State<'_, ProfileCache>,
    schema_scan_cache: State<'_, SchemaScanCache>,
    dedup_cache: State<'_, DedupCache>,
    compare_cache: State<'_, CompareCache>,
    cluster_cache: State<'_, ClusterCache>,
    semantic_cache: State<'_, SemanticCache>,
    crossval_cache: State<'_, CrossValCache>,
    outlier_cache: State<'_, OutlierCache>,
    append_cache: State<'_, AppendCache>,
    pii_cache: State<'_, PiiCache>,
    annotations: State<'_, AnnotationRegistry>,
    follows: State<'_, FollowRegistry>,
) -> AppResult<()> {
    // Closing a followed tab stops its watcher and releases the handle.
    follows.stop(doc_id);
    // A clean close deletes the crash-recovery journal (F16): there is
    // nothing left to recover once the user closed the tab deliberately.
    if let Ok(handle) = doc_handle(&state, doc_id) {
        if let Ok(mut doc) = handle.write() {
            if let Some(journal) = doc.take_journal() {
                journal.delete();
            }
        }
    }
    lock(&state)?.remove(doc_id);
    diagnostics_cache.remove(doc_id);
    profile_cache.remove_doc(doc_id);
    schema_scan_cache.remove_doc(doc_id);
    dedup_cache.remove(doc_id);
    compare_cache.remove_doc(doc_id);
    cluster_cache.remove(doc_id);
    semantic_cache.remove(doc_id);
    crossval_cache.remove(doc_id);
    outlier_cache.remove(doc_id);
    append_cache.remove(doc_id);
    pii_cache.remove(doc_id);
    annotations.remove(doc_id);
    Ok(())
}

// ----- fuzzy value clustering (F24) ------------------------------------------

/// The last completed cluster report for a document, if any. Carries the
/// revision it was computed against; the UI offers a rescan when stale.
#[tauri::command]
pub fn get_cluster_report(
    doc_id: u64,
    cluster_cache: State<'_, ClusterCache>,
) -> Option<ClusterReport> {
    cluster_cache.get(doc_id)
}

/// Start a clustering scan as a cancellable job (F24). The report lands in
/// the cluster cache; nothing is ever applied automatically.
#[tauri::command]
pub async fn start_cluster_scan(
    doc_id: u64,
    spec: ClusterSpec,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
    cluster_cache: State<'_, ClusterCache>,
) -> AppResult<u64> {
    let handle = doc_handle(&state, doc_id)?;
    {
        let doc = handle.read().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
    }
    let sink = cluster_cache.share();
    let ctx = jobs.begin_for_app(&app, "cluster", Some(doc_id));
    let job_id = ctx.id;
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let doc = handle.read().map_err(poisoned)?;
            doc.check_revision(expected_revision)?;
            let report = cluster::scan(&doc, &spec, ctx)?;
            if let Ok(mut map) = sink.lock() {
                map.insert(doc_id, report);
            }
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

/// Apply the ACCEPTED cluster mappings as ONE undoable operation (F24),
/// guarded by the revision the report was computed against.
#[tauri::command]
pub fn apply_value_clusters(
    doc_id: u64,
    column: usize,
    mapping: Vec<(String, String)>,
    scope: ExportScope,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        doc.ensure_editable()?;
        doc.check_revision(expected_revision)?;
        let changes = cluster::mapping_changes(doc, column, &mapping, &scope)?;
        doc.set_cells(changes)?;
        Ok(doc.meta())
    })
}

// ----- semantic data-type detection (F26) ------------------------------------

/// The last completed semantic report for a document, if any. Carries the
/// revision it was computed against and whether it came from a sample.
#[tauri::command]
pub fn get_semantic_report(
    doc_id: u64,
    semantic_cache: State<'_, SemanticCache>,
) -> Option<SemanticReport> {
    semantic_cache.get(doc_id)
}

/// Start a semantic-type scan over every column as a cancellable job (F26).
/// Detection is strictly read-only; large indexed documents are sampled and
/// the report says so.
#[tauri::command]
pub async fn start_semantic_scan(
    doc_id: u64,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
    semantic_cache: State<'_, SemanticCache>,
) -> AppResult<u64> {
    let handle = doc_handle(&state, doc_id)?;
    {
        let doc = handle.read().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
    }
    let sink = semantic_cache.share();
    let ctx = jobs.begin_for_app(&app, "semantic", Some(doc_id));
    let job_id = ctx.id;
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let doc = handle.read().map_err(poisoned)?;
            doc.check_revision(expected_revision)?;
            let report = semantic::scan(&doc, ctx)?;
            if let Ok(mut map) = sink.lock() {
                map.insert(doc_id, report);
            }
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

/// Filter the grid to rows whose cell in `column` is valid (or invalid) for
/// a semantic type. Blank cells match neither filter.
#[tauri::command]
pub async fn apply_semantic_filter(
    doc_id: u64,
    column: usize,
    semantic: SemanticType,
    keep_valid: bool,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        // Compute against a consistent read snapshot first…
        let rows = {
            let doc = handle.read().map_err(poisoned)?;
            doc.check_revision(expected_revision)?;
            semantic::semantic_rows(&doc, column, semantic, keep_valid)?
        };
        // …then swap the filter view in (re-checked: an edit may have raced).
        let mut doc = handle.write().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
        doc.set_filter(rows)?;
        Ok(doc.meta())
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Preview exactly what a semantic quick action would change — counts plus
/// leading before/after examples. Nothing is mutated.
#[tauri::command]
pub async fn preview_semantic_action(
    doc_id: u64,
    column: usize,
    semantic: SemanticType,
    action: SemanticAction,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<SemanticActionPreview> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let doc = handle.read().map_err(poisoned)?;
        // Actions mutate, so previews are for editable documents only.
        doc.ensure_editable()?;
        doc.check_revision(expected_revision)?;
        semantic::preview_action(&doc, column, semantic, action)
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Apply a previewed semantic action as ONE undoable operation, guarded by
/// the preview's revision. In-place actions commit via `set_cells`; the
/// extraction actions insert a new column right after the source.
#[tauri::command]
pub async fn apply_semantic_action(
    doc_id: u64,
    column: usize,
    semantic: SemanticType,
    action: SemanticAction,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut doc = handle.write().map_err(poisoned)?;
        doc.ensure_editable()?;
        doc.check_revision(expected_revision)?;
        let (changes, new_column) = semantic::action_changes(&doc, column, semantic, action)?;
        match new_column {
            Some((name, values)) => {
                doc.replace_columns(Vec::new(), column + 1, vec![(name, values)])?;
            }
            None => doc.set_cells(changes)?,
        }
        Ok(doc.meta())
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

// ----- cross-column validation (F27) -----------------------------------------

/// The last completed cross-validation report (with the rules it ran).
#[tauri::command]
pub fn get_crossval_report(
    doc_id: u64,
    crossval_cache: State<'_, CrossValCache>,
) -> Option<(Vec<CrossRule>, CrossValReport)> {
    crossval_cache.get(doc_id)
}

/// Run cross-column rules as a cancellable job (F27). Rule configurations
/// are validated (shape + column resolution) before any row is read.
#[tauri::command]
pub async fn start_crossval_scan(
    doc_id: u64,
    rules: Vec<CrossRule>,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
    crossval_cache: State<'_, CrossValCache>,
) -> AppResult<u64> {
    crossval::validate_rules(&rules)?;
    let handle = doc_handle(&state, doc_id)?;
    {
        let doc = handle.read().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
    }
    let sink = crossval_cache.share();
    let ctx = jobs.begin_for_app(&app, "crossval", Some(doc_id));
    let job_id = ctx.id;
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let doc = handle.read().map_err(poisoned)?;
            doc.check_revision(expected_revision)?;
            let report = crossval::scan(&doc, &rules, ctx)?;
            if let Ok(mut map) = sink.lock() {
                map.insert(doc_id, (rules, report));
            }
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

/// Filter the grid to rows violating one rule (`rule`) or any rule (None),
/// guarded by the report's revision so stale results cannot be applied.
#[tauri::command]
pub async fn apply_crossval_filter(
    doc_id: u64,
    rules: Vec<CrossRule>,
    rule: Option<usize>,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let rows = {
            let doc = handle.read().map_err(poisoned)?;
            doc.check_revision(expected_revision)?;
            crossval::violating_rows(&doc, &rules, rule)?
        };
        let mut doc = handle.write().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
        doc.set_filter(rows)?;
        Ok(doc.meta())
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

// ----- missing-value repair (F29) --------------------------------------------

/// Preview exactly what a repair would do — affected cells, removals, the
/// computed fill values, and leading before/after examples. Never mutates.
#[tauri::command]
pub async fn preview_repair(
    doc_id: u64,
    spec: RepairSpec,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<RepairPreview> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let doc = handle.read().map_err(poisoned)?;
        doc.ensure_editable()?;
        doc.check_revision(expected_revision)?;
        Ok(repair::compute(&doc, &spec)?.preview)
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Apply a previewed repair as ONE undoable operation, guarded by the
/// preview's revision. Cell fills commit via `set_cells`; the removal
/// operations delete whole rows/columns (explicitly confirmed in the UI).
#[tauri::command]
pub async fn apply_repair(
    doc_id: u64,
    spec: RepairSpec,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut doc = handle.write().map_err(poisoned)?;
        doc.ensure_editable()?;
        doc.check_revision(expected_revision)?;
        let computed = repair::compute(&doc, &spec)?;
        if !computed.remove_rows.is_empty() {
            doc.delete_rows(computed.remove_rows)?;
        } else if !computed.remove_columns.is_empty() {
            doc.delete_columns(computed.remove_columns)?;
        } else {
            doc.set_cells(computed.changes)?;
        }
        Ok(doc.meta())
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

// ----- outlier and anomaly finder (F30) --------------------------------------

/// The last completed outlier report + the spec that produced it.
#[tauri::command]
pub fn get_outlier_report(
    doc_id: u64,
    outlier_cache: State<'_, OutlierCache>,
) -> Option<CachedOutlier> {
    outlier_cache.get(doc_id)
}

/// Run an outlier scan as a cancellable job (F30). Read-only — scanning
/// never marks the document dirty.
#[tauri::command]
pub async fn start_outlier_scan(
    doc_id: u64,
    spec: OutlierSpec,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
    outlier_cache: State<'_, OutlierCache>,
) -> AppResult<u64> {
    let handle = doc_handle(&state, doc_id)?;
    {
        let doc = handle.read().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
    }
    let sink = outlier_cache.share();
    let ctx = jobs.begin_for_app(&app, "outlier", Some(doc_id));
    let job_id = ctx.id;
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let doc = handle.read().map_err(poisoned)?;
            doc.check_revision(expected_revision)?;
            let report = outlier::scan(&doc, &spec, ctx)?;
            if let Ok(mut map) = sink.lock() {
                map.insert(doc_id, (spec, report));
            }
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

/// Filter the grid to the rows holding flagged values, guarded by the
/// report's revision (a stale report cannot be applied).
#[tauri::command]
pub async fn apply_outlier_filter(
    doc_id: u64,
    spec: OutlierSpec,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let rows = {
            let doc = handle.read().map_err(poisoned)?;
            doc.check_revision(expected_revision)?;
            outlier::flagged_rows(&doc, &spec)?
        };
        let mut doc = handle.write().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
        doc.set_filter(rows)?;
        Ok(doc.meta())
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Preview a corrective action — counts plus before/after examples.
#[tauri::command]
pub async fn preview_outlier_action(
    doc_id: u64,
    spec: OutlierSpec,
    action: OutlierAction,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<OutlierActionPreview> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let doc = handle.read().map_err(poisoned)?;
        doc.ensure_editable()?;
        doc.check_revision(expected_revision)?;
        Ok(outlier::action_changes(&doc, &spec, action)?.preview)
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Apply a previewed corrective action as ONE undoable operation.
#[tauri::command]
pub async fn apply_outlier_action(
    doc_id: u64,
    spec: OutlierSpec,
    action: OutlierAction,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut doc = handle.write().map_err(poisoned)?;
        doc.ensure_editable()?;
        doc.check_revision(expected_revision)?;
        let computed = outlier::action_changes(&doc, &spec, action)?;
        if !computed.remove_rows.is_empty() {
            // Row removal shifts the absolute indices an active row view
            // refers to — drop it first, like every structural delete path.
            doc.clear_row_view();
            doc.delete_rows(computed.remove_rows)?;
        } else {
            doc.set_cells(computed.changes)?;
        }
        Ok(doc.meta())
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

// ----- multi-file append (F20) ------------------------------------------------

/// Resolve front-end append inputs: open documents to their shared handles
/// (with their display names), files to validated paths.
fn resolve_append_inputs(
    state: &Db<'_>,
    inputs: Vec<AppendInput>,
) -> AppResult<Vec<append::ResolvedInput>> {
    if inputs.is_empty() {
        return Err(AppError::invalid("pick at least one input"));
    }
    let mut resolved = Vec::with_capacity(inputs.len());
    for input in inputs {
        match input {
            AppendInput::OpenDoc { doc_id } => {
                let handle = doc_handle(state, doc_id)?;
                let name = {
                    let doc = handle.read().map_err(poisoned)?;
                    doc.meta().file_name
                };
                resolved.push(append::ResolvedInput::Doc { name, doc: handle });
            }
            AppendInput::File { path } => {
                let path = PathBuf::from(path);
                if !path.is_file() {
                    return Err(AppError::invalid(format!("not a file: {}", path.display())));
                }
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                resolved.push(append::ResolvedInput::File { name, path });
            }
        }
    }
    Ok(resolved)
}

/// Preview an append: output schema, per-input mappings and warnings,
/// projected rows and backing. Nothing is created.
#[tauri::command]
pub async fn preview_append(
    inputs: Vec<AppendInput>,
    options: AppendOptions,
    state: Db<'_>,
) -> AppResult<AppendPreview> {
    let resolved = resolve_append_inputs(&state, inputs)?;
    tauri::async_runtime::spawn_blocking(move || append::preview(&resolved, &options))
        .await
        .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Run the append as a cancellable job (kind "derive"). The NEW document
/// registers under the returned doc id when the job finishes; input tabs
/// and files are never modified.
#[tauri::command]
pub async fn start_append(
    inputs: Vec<AppendInput>,
    options: AppendOptions,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
    append_cache: State<'_, AppendCache>,
) -> AppResult<IndexedOpenStart> {
    let resolved = resolve_append_inputs(&state, inputs)?;
    let doc_id = lock(&state)?.alloc_id();
    let cache_root = index_cache_root(&app)?;
    let sink = append_cache.share();

    let ctx = jobs.begin_for_app(&app, "derive", Some(doc_id));
    let job_id = ctx.id;
    let app_for_job = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            use tauri::Manager;
            let (doc, report) = append::run(&resolved, &options, doc_id, cache_root, ctx)?;
            if let Ok(mut map) = sink.lock() {
                map.insert(doc_id, report);
            }
            let registry = app_for_job.state::<Mutex<AppState>>();
            registry
                .lock()
                .map_err(|_| AppError::Other("internal state lock error".into()))?
                .insert(doc);
            Ok(())
        })
        .await;
    });
    Ok(IndexedOpenStart { job_id, doc_id })
}

/// The per-input outcome report of a finished append.
#[tauri::command]
pub fn get_append_report(
    doc_id: u64,
    append_cache: State<'_, AppendCache>,
) -> Option<AppendReport> {
    append_cache.get(doc_id)
}

// ----- group-by aggregations (F22) ---------------------------------------------

/// Preview a group-by: schema, group count, ignored-invalid counts, and
/// sample output rows. Nothing is created.
#[tauri::command]
pub async fn preview_group_by(
    doc_id: u64,
    spec: GroupBySpec,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<GroupByPreview> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let doc = handle.read().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
        groupby::preview(&doc, &spec)
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Run a group-by as a cancellable "derive" job into a NEW document (F22).
#[tauri::command]
pub async fn start_group_by(
    doc_id: u64,
    spec: GroupBySpec,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
) -> AppResult<IndexedOpenStart> {
    let handle = doc_handle(&state, doc_id)?;
    let new_doc_id = lock(&state)?.alloc_id();
    let cache_root = index_cache_root(&app)?;

    let ctx = jobs.begin_for_app(&app, "derive", Some(new_doc_id));
    let job_id = ctx.id;
    let app_for_job = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            use tauri::Manager;
            let source = handle.read().map_err(poisoned)?;
            source.check_revision(expected_revision)?;
            let doc = groupby::run(&source, &spec, new_doc_id, cache_root, ctx)?;
            drop(source);
            let registry = app_for_job.state::<Mutex<AppState>>();
            registry
                .lock()
                .map_err(|_| AppError::Other("internal state lock error".into()))?
                .insert(doc);
            Ok(())
        })
        .await;
    });
    Ok(IndexedOpenStart {
        job_id,
        doc_id: new_doc_id,
    })
}

// ----- crash recovery (F16) -------------------------------------------------------

/// Recoverable sessions found at startup (expired journals swept first).
#[tauri::command]
pub fn list_recovery_sessions(app: tauri::AppHandle) -> AppResult<Vec<RecoverableSession>> {
    let dir = recovery_dir(&app)?;
    let settings = settings::load_settings(&settings_dir(&app)?);
    journal::sweep_expired(&dir, settings.recovery_retention_days.max(1));
    Ok(journal::scan_recoverable(&dir))
}

/// Guard: a journal path handed back by the UI must live INSIDE the
/// recovery directory — never delete or read arbitrary files.
fn checked_journal_path(app: &tauri::AppHandle, journal_path: &str) -> AppResult<PathBuf> {
    let dir = recovery_dir(app)?;
    let path = PathBuf::from(journal_path);
    let canonical_dir = dir
        .canonicalize()
        .map_err(|_| AppError::invalid("no recovery data"))?;
    let canonical = path
        .canonicalize()
        .map_err(|_| AppError::invalid("that recovery session no longer exists"))?;
    if !canonical.starts_with(&canonical_dir) {
        return Err(AppError::invalid("not a recovery journal"));
    }
    Ok(canonical)
}

/// Recover a journaled session: reparse the source with the journal's
/// interpretation, replay the operations, and register the result as a
/// DIRTY document. The source file is never written. `open_copy` recovers
/// into an unsaved copy (required when the source changed underneath).
#[tauri::command]
pub async fn recover_session(
    journal_path: String,
    open_copy: bool,
    app: tauri::AppHandle,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let journal_file = checked_journal_path(&app, &journal_path)?;
    let (header, records) = journal::read_journal(&journal_file)?;
    let source = PathBuf::from(&header.path);
    if !source.is_file() {
        return Err(AppError::invalid(
            "the source file no longer exists — use Show Location to find the journal",
        ));
    }
    let disk = util::stat_fingerprint(&source);
    let changed = match (&header.fingerprint, &disk) {
        (Some(a), Some(b)) => a != b,
        _ => true,
    };
    if changed && !open_copy {
        return Err(AppError::invalid(
            "the source file changed since journaling — blind replay is \
             blocked; recover as a copy instead",
        ));
    }

    let delimiter = header.delimiter.clone();
    let encoding_name = header.encoding.clone();
    let source_for_parse = source.clone();
    let (parsed, fingerprint) = parse_file(
        source_for_parse,
        Some(util::delimiter_to_byte(&delimiter)),
        Some(encoding::from_name(&encoding_name)),
    )
    .await?;

    let id = lock(&state)?.alloc_id();
    let mut doc = Document::from_parsed(
        id,
        (!open_copy).then(|| source.clone()),
        parsed,
        header.has_header_row,
    );
    if !open_copy {
        doc.set_fingerprint(fingerprint);
        // Journal onward from the recovered baseline BEFORE replay, so the
        // replayed operations are immediately protected again.
        attach_journal_if_enabled(&app, &mut doc);
    } else {
        doc.mark_derived_unsaved();
    }
    let applied = tauri::async_runtime::spawn_blocking(move || -> AppResult<(Document, usize)> {
        let applied = doc.replay_journal_records(&records)?;
        Ok((doc, applied))
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?;
    let (doc, _applied) = applied?;
    let meta = doc.meta();
    lock(&state)?.insert(doc);
    // Replay succeeded and the work now lives in a (journaled) document:
    // the old journal is done.
    let _ = std::fs::remove_file(&journal_file);
    Ok(meta)
}

/// Discard one recovery session (deletes its journal).
#[tauri::command]
pub fn discard_recovery_session(journal_path: String, app: tauri::AppHandle) -> AppResult<()> {
    let path = checked_journal_path(&app, &journal_path)?;
    std::fs::remove_file(path)?;
    Ok(())
}

/// "Delete all recovery data": wipe every journal.
#[tauri::command]
pub fn delete_all_recovery(app: tauri::AppHandle) -> AppResult<usize> {
    Ok(journal::delete_all(&recovery_dir(&app)?))
}

// ----- change inspector / selective revert (F15) ---------------------------------

/// Unsaved operations plus whether the saved state sits in the REDO branch
/// (the user undid past the last save — nothing to list, but the document
/// is dirty and Redo returns to the saved state).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangesReport {
    pub saved_in_redo: bool,
    pub changes: Vec<ChangeSummary>,
}

/// Every unsaved operation, oldest first, with cell-level samples.
#[tauri::command]
pub fn get_changes(doc_id: u64, state: Db<'_>) -> AppResult<ChangesReport> {
    read_doc(&state, doc_id, |doc| {
        Ok(ChangesReport {
            saved_in_redo: doc.saved_in_redo_branch(),
            changes: doc.changes_since_save(),
        })
    })
}

/// Revert one whole operation (as a NEW, undoable operation).
#[tauri::command]
pub fn revert_change(
    doc_id: u64,
    op_id: u64,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        doc.check_revision(expected_revision)?;
        doc.revert_stack_op(op_id)?;
        Ok(doc.meta())
    })
}

/// Revert specific cells of one cell-edit operation.
#[tauri::command]
pub fn revert_change_cells(
    doc_id: u64,
    op_id: u64,
    cells: Vec<(usize, usize)>,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        doc.check_revision(expected_revision)?;
        doc.revert_cells_of_op(op_id, &cells)?;
        Ok(doc.meta())
    })
}

/// Revert every unsaved edit in one column to its value at the last save.
#[tauri::command]
pub fn revert_column_changes(
    doc_id: u64,
    col: usize,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        doc.check_revision(expected_revision)?;
        doc.revert_column_edits(col)?;
        Ok(doc.meta())
    })
}

/// Revert EVERYTHING since the last save as one undoable operation.
#[tauri::command]
pub fn revert_all_changes(
    doc_id: u64,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        doc.check_revision(expected_revision)?;
        doc.revert_all_changes()?;
        Ok(doc.meta())
    })
}

// ----- PII detection and redaction (F28) -----------------------------------------

/// Append one line to the LOCAL-ONLY audit log: counts and kinds, never
/// values. Best-effort — a failing audit write never blocks a redaction.
fn append_pii_audit(app: &tauri::AppHandle, line: &str) {
    use tauri::Manager;
    let Ok(dir) = app.path().app_data_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("pii-audit.log");
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = writeln!(file, "{epoch} {line}");
    }
}

/// The last completed PII report + the spec that produced it.
#[tauri::command]
pub fn get_pii_report(doc_id: u64, pii_cache: State<'_, PiiCache>) -> Option<CachedPii> {
    pii_cache.get(doc_id)
}

/// Run a PII scan as a cancellable job (F28). Read-only; report samples
/// are masked — raw values never leave the scan.
#[tauri::command]
pub async fn start_pii_scan(
    doc_id: u64,
    spec: PiiSpec,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
    pii_cache: State<'_, PiiCache>,
) -> AppResult<u64> {
    let handle = doc_handle(&state, doc_id)?;
    {
        let doc = handle.read().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
    }
    let sink = pii_cache.share();
    let ctx = jobs.begin_for_app(&app, "pii", Some(doc_id));
    let job_id = ctx.id;
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let doc = handle.read().map_err(poisoned)?;
            doc.check_revision(expected_revision)?;
            let report = pii::scan(&doc, &spec, ctx)?;
            if let Ok(mut map) = sink.lock() {
                map.insert(doc_id, (spec, report));
            }
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

/// Preview a redaction: affected counts and MASKED before/after examples.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn preview_redaction(
    doc_id: u64,
    spec: PiiSpec,
    detector: usize,
    column: usize,
    action: RedactionAction,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<RedactionPreview> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let doc = handle.read().map_err(poisoned)?;
        doc.ensure_editable()?;
        doc.check_revision(expected_revision)?;
        Ok(pii::redaction_changes(&doc, &spec, detector, column, &action)?.preview)
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Apply a previewed redaction as ONE undoable operation, and append a
/// value-free line to the local audit log.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn apply_redaction(
    doc_id: u64,
    spec: PiiSpec,
    detector: usize,
    column: usize,
    action: RedactionAction,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    let handle = doc_handle(&state, doc_id)?;
    let result =
        tauri::async_runtime::spawn_blocking(move || -> AppResult<(DocumentMeta, String)> {
            let mut doc = handle.write().map_err(poisoned)?;
            doc.ensure_editable()?;
            doc.check_revision(expected_revision)?;
            let computed = pii::redaction_changes(&doc, &spec, detector, column, &action)?;
            let affected;
            if let Some(col) = computed.remove_column {
                affected = 1;
                doc.delete_columns(vec![col])?;
            } else if !computed.remove_rows.is_empty() {
                affected = computed.remove_rows.len();
                doc.delete_rows(computed.remove_rows)?;
            } else {
                affected = computed.changes.len();
                doc.set_cells(computed.changes)?;
            }
            let detector_label = spec
                .detectors
                .get(detector)
                .map(|d| d.label())
                .unwrap_or_default();
            let action_kind = match action {
                RedactionAction::FixedReplacement { .. } => "fixedReplacement",
                RedactionAction::KeepLast { .. } => "keepLast",
                RedactionAction::FullMask => "fullMask",
                RedactionAction::Pseudonymize { .. } => "pseudonymize",
                RedactionAction::RemoveColumn => "removeColumn",
                RedactionAction::RemoveRows => "removeRows",
            };
            let audit = format!(
                "doc=\"{}\" detector=\"{detector_label}\" column={column} \
             action={action_kind} affected={affected}",
                doc.meta().file_name
            );
            Ok((doc.meta(), audit))
        })
        .await
        .map_err(|e| AppError::Other(format!("background task failed: {e}")))??;
    let (meta, audit) = result;
    append_pii_audit(&app, &audit);
    Ok(meta)
}

// ----- data dictionary (F38) ----------------------------------------------------

/// The dictionary editor surface: one row per current column (technical name +
/// inferred F31 type prefilled, stored entry when documented) plus any orphaned
/// entries. `dictionaryRevision` is the metadata revision used to guard edits;
/// documentation edits never move the document `revision` or the dirty flag.
#[tauri::command]
pub fn get_dictionary(doc_id: u64, state: Db<'_>) -> AppResult<DictionaryView> {
    read_doc(&state, doc_id, |doc| Ok(dictionary::view(doc)))
}

/// Insert or replace one column's documentation. An entry with no populated
/// field is removed rather than stored empty. Metadata only: not undoable,
/// never dirties the document. Guarded by the dictionary revision.
#[tauri::command]
pub fn set_dictionary_field(
    doc_id: u64,
    field: DictionaryField,
    expected_dictionary_revision: u64,
    state: Db<'_>,
) -> AppResult<DictionaryView> {
    write_doc(&state, doc_id, |doc| {
        doc.check_dictionary_revision(expected_dictionary_revision)?;
        dictionary::validate_field(&field)?;
        // The entry must key on a column that exists (present or orphaned is
        // fine — it is keyed by stable ID either way).
        if field.is_documented() {
            doc.set_dictionary_field(field);
        } else {
            doc.remove_dictionary_field(&field.column_id);
        }
        Ok(dictionary::view(doc))
    })
}

/// Drop one column's documentation entry (clearing a column, or discarding a
/// single orphan). Guarded by the dictionary revision.
#[tauri::command]
pub fn remove_dictionary_field(
    doc_id: u64,
    column_id: String,
    expected_dictionary_revision: u64,
    state: Db<'_>,
) -> AppResult<DictionaryView> {
    write_doc(&state, doc_id, |doc| {
        doc.check_dictionary_revision(expected_dictionary_revision)?;
        doc.remove_dictionary_field(&column_id);
        Ok(dictionary::view(doc))
    })
}

/// Discard EVERY orphaned entry (documentation whose column is gone). The
/// user's explicit "clean up orphans" action. Guarded by the dictionary
/// revision.
#[tauri::command]
pub fn discard_dictionary_orphans(
    doc_id: u64,
    expected_dictionary_revision: u64,
    state: Db<'_>,
) -> AppResult<DictionaryView> {
    write_doc(&state, doc_id, |doc| {
        doc.check_dictionary_revision(expected_dictionary_revision)?;
        let orphan_ids: Vec<String> = dictionary::orphans(doc)
            .into_iter()
            .map(|o| o.column_id)
            .collect();
        if !orphan_ids.is_empty() {
            let mut dict = doc.dictionary().clone();
            for id in &orphan_ids {
                dict.remove(id);
            }
            doc.set_dictionary(dict);
        }
        Ok(dictionary::view(doc))
    })
}

/// Export the dictionary as versioned JSON, Markdown documentation, or tabular
/// CSV documentation (atomic write via the F03 pipeline).
#[tauri::command]
pub async fn export_dictionary(
    doc_id: u64,
    path: String,
    format: DictionaryFormat,
    state: Db<'_>,
) -> AppResult<()> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let rendered = {
            let doc = handle.read().map_err(poisoned)?;
            dictionary::export_as(&doc, format)?
        };
        save_mod::atomic_write(Path::new(&path), BackupPolicy::None, |file| {
            use std::io::Write;
            file.write_all(rendered.as_bytes())?;
            Ok(rendered.len() as u64)
        })?;
        Ok(())
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Plan a dictionary import: parse the CEESVEE dictionary JSON at `path`, match
/// its entries to current columns by ID or mapped name, and return the merge
/// plan (clean additions + the field-level conflicts that must be resolved
/// before applying). Read-only — nothing changes.
#[tauri::command]
pub async fn preview_dictionary_import(
    doc_id: u64,
    path: String,
    match_by: MergeMatchBy,
    state: Db<'_>,
) -> AppResult<MergePlan> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let json = std::fs::read_to_string(&path)?;
        let imported = dictionary::parse_import(&json)?;
        let doc = handle.read().map_err(poisoned)?;
        Ok(dictionary::plan_merge(&doc, &imported, match_by))
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Apply a dictionary import under an explicit conflict resolution. Fails
/// (changing nothing) if any reported conflict is left unresolved, or if the
/// dictionary moved since the plan was taken. Metadata only: never dirties the
/// document.
#[tauri::command]
pub async fn apply_dictionary_import(
    doc_id: u64,
    path: String,
    match_by: MergeMatchBy,
    resolution: MergeResolution,
    expected_dictionary_revision: u64,
    state: Db<'_>,
) -> AppResult<DictionaryImportOutcome> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let json = std::fs::read_to_string(&path)?;
        let imported = dictionary::parse_import(&json)?;
        let mut doc = handle.write().map_err(poisoned)?;
        doc.check_dictionary_revision(expected_dictionary_revision)?;
        let applied = dictionary::apply_merge(&doc, &imported, match_by, &resolution)?;
        doc.set_dictionary(applied.dictionary);
        Ok(DictionaryImportOutcome {
            matched_columns: applied.matched_columns,
            new_entries: applied.new_entries,
            updated_entries: applied.updated_entries,
            fields_added: applied.fields_added,
            conflicts_resolved: applied.conflicts_resolved,
            unmatched: applied.unmatched,
            view: dictionary::view(&doc),
        })
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

// ----- row bookmarks, tags and notes (F40) ---------------------------------------
//
// Annotations live in the doc_id-keyed [`AnnotationRegistry`], deliberately
// OUTSIDE the document: they survive the whole-`Document` replacement a reparse
// / reindex / convert-to-editable performs (the id is stable) and are re-resolved
// lazily against the current content on every read, so those flows need no change
// — the front end calls `annotations_rematch` after a reload to surface the
// ambiguous / orphaned review list. All edits are metadata: never undoable, never
// dirtying the document, guarded by the store's own revision. Reads run a bounded
// scan of the document (like `apply_diagnostic_filter`); a huge indexed source is
// only ever fully scanned when a record anchor's content drifted (which an
// immutable indexed backing never does) or a key spec is in use.

/// Run `f` with read access to the document and mutable access to its
/// annotation store (both created/looked-up here). Lock order is always
/// document-then-registry.
fn edit_annotations<T>(
    state: &Db<'_>,
    annotations: &State<'_, AnnotationRegistry>,
    doc_id: u64,
    f: impl FnOnce(&Document, &mut annotations::AnnotationStore) -> AppResult<T>,
) -> AppResult<T> {
    let handle = doc_handle(state, doc_id)?;
    let doc = handle.read().map_err(poisoned)?;
    annotations.try_with(doc_id, |store| f(&doc, store))
}

/// Snapshot the annotations panel surface (rematched against the current doc).
#[tauri::command]
pub fn annotations_view(
    doc_id: u64,
    state: Db<'_>,
    annotations: State<'_, AnnotationRegistry>,
) -> AppResult<AnnotationsView> {
    edit_annotations(&state, &annotations, doc_id, |doc, store| {
        store.view(&DocumentSource::new(doc), doc.revision(), None)
    })
}

/// Re-resolve every annotation against the current document and return the
/// matched tally plus the ambiguous / orphaned review list. The front end calls
/// this after any reparse / reindex / external-change reload.
#[tauri::command]
pub fn annotations_rematch(
    doc_id: u64,
    state: Db<'_>,
    annotations: State<'_, AnnotationRegistry>,
) -> AppResult<RematchReport> {
    edit_annotations(&state, &annotations, doc_id, |doc, store| {
        store.rematch_report(&DocumentSource::new(doc), None)
    })
}

/// Set (or clear, with `None`) the key columns used to anchor NEW annotations,
/// then re-anchor the matched existing ones to the new mechanism.
#[tauri::command]
pub fn annotations_set_key_spec(
    doc_id: u64,
    key_spec: Option<KeySpec>,
    expected_annotations_revision: u64,
    state: Db<'_>,
    annotations: State<'_, AnnotationRegistry>,
) -> AppResult<AnnotationsView> {
    edit_annotations(&state, &annotations, doc_id, |doc, store| {
        store.check_revision(expected_annotations_revision)?;
        let source = DocumentSource::new(doc);
        store.set_key_spec(key_spec);
        store.reanchor(&source, None)?;
        store.view(&source, doc.revision(), None)
    })
}

/// Set (or clear, with `None`) the default author label carried on new notes.
#[tauri::command]
pub fn annotations_set_author(
    doc_id: u64,
    author: Option<String>,
    expected_annotations_revision: u64,
    state: Db<'_>,
    annotations: State<'_, AnnotationRegistry>,
) -> AppResult<AnnotationsView> {
    edit_annotations(&state, &annotations, doc_id, |doc, store| {
        store.check_revision(expected_annotations_revision)?;
        store.set_author(author);
        store.view(&DocumentSource::new(doc), doc.revision(), None)
    })
}

/// Star / flag / add or remove tags on the row at `display_row` (translated to
/// its absolute record). Creates the annotation if absent; prunes it if empty.
#[tauri::command]
pub fn annotations_edit_row(
    doc_id: u64,
    display_row: usize,
    patch: RowMarkPatch,
    expected_annotations_revision: u64,
    state: Db<'_>,
    annotations: State<'_, AnnotationRegistry>,
) -> AppResult<AnnotationsView> {
    edit_annotations(&state, &annotations, doc_id, |doc, store| {
        store.check_revision(expected_annotations_revision)?;
        let record = abs_row(doc, display_row)? as u64;
        let source = DocumentSource::new(doc);
        store.edit_row_marks(&source, record, &patch, None)?;
        store.view(&source, doc.revision(), None)
    })
}

/// Set (or clear, with `text = None`) the ROW note on `display_row`.
#[tauri::command]
pub fn annotations_set_row_note(
    doc_id: u64,
    display_row: usize,
    text: Option<String>,
    author: Option<String>,
    expected_annotations_revision: u64,
    state: Db<'_>,
    annotations: State<'_, AnnotationRegistry>,
) -> AppResult<AnnotationsView> {
    edit_annotations(&state, &annotations, doc_id, |doc, store| {
        store.check_revision(expected_annotations_revision)?;
        let record = abs_row(doc, display_row)? as u64;
        let source = DocumentSource::new(doc);
        store.set_row_note(&source, record, text, author, None)?;
        store.view(&source, doc.revision(), None)
    })
}

/// Set (or clear, with `text = None`) a CELL note on `column_id` of
/// `display_row`.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn annotations_set_cell_note(
    doc_id: u64,
    display_row: usize,
    column_id: String,
    text: Option<String>,
    author: Option<String>,
    expected_annotations_revision: u64,
    state: Db<'_>,
    annotations: State<'_, AnnotationRegistry>,
) -> AppResult<AnnotationsView> {
    edit_annotations(&state, &annotations, doc_id, |doc, store| {
        store.check_revision(expected_annotations_revision)?;
        let record = abs_row(doc, display_row)? as u64;
        let source = DocumentSource::new(doc);
        store.set_cell_note(&source, record, &column_id, text, author, None)?;
        store.view(&source, doc.revision(), None)
    })
}

/// Delete one whole annotation entry by its stable handle (e.g. discarding a
/// single orphan from the review list).
#[tauri::command]
pub fn annotations_remove_row(
    doc_id: u64,
    handle: u64,
    expected_annotations_revision: u64,
    state: Db<'_>,
    annotations: State<'_, AnnotationRegistry>,
) -> AppResult<AnnotationsView> {
    edit_annotations(&state, &annotations, doc_id, |doc, store| {
        store.check_revision(expected_annotations_revision)?;
        store.remove_row(handle);
        store.view(&DocumentSource::new(doc), doc.revision(), None)
    })
}

/// Discard every orphaned annotation (no matching row in the current document).
#[tauri::command]
pub fn annotations_discard_orphans(
    doc_id: u64,
    expected_annotations_revision: u64,
    state: Db<'_>,
    annotations: State<'_, AnnotationRegistry>,
) -> AppResult<AnnotationsView> {
    edit_annotations(&state, &annotations, doc_id, |doc, store| {
        store.check_revision(expected_annotations_revision)?;
        let source = DocumentSource::new(doc);
        store.discard_orphans(&source, None)?;
        store.view(&source, doc.revision(), None)
    })
}

/// Define or update a tag in the namespace.
#[tauri::command]
pub fn annotations_define_tag(
    doc_id: u64,
    tag: TagDef,
    expected_annotations_revision: u64,
    state: Db<'_>,
    annotations: State<'_, AnnotationRegistry>,
) -> AppResult<AnnotationsView> {
    edit_annotations(&state, &annotations, doc_id, |doc, store| {
        store.check_revision(expected_annotations_revision)?;
        store.define_tag(tag)?;
        store.view(&DocumentSource::new(doc), doc.revision(), None)
    })
}

/// Remove a tag from the namespace and from every row that carries it.
#[tauri::command]
pub fn annotations_remove_tag(
    doc_id: u64,
    name: String,
    expected_annotations_revision: u64,
    state: Db<'_>,
    annotations: State<'_, AnnotationRegistry>,
) -> AppResult<AnnotationsView> {
    edit_annotations(&state, &annotations, doc_id, |doc, store| {
        store.check_revision(expected_annotations_revision)?;
        store.remove_tag(&name);
        store.view(&DocumentSource::new(doc), doc.revision(), None)
    })
}

/// Filter the grid to the rows matching an annotation-state predicate
/// (starred / flagged / tagged / has-note …), via the existing row-filter view.
/// Only MATCHED rows contribute — an ambiguous or orphaned annotation is never
/// filtered onto. Guarded by the document revision.
#[tauri::command]
pub fn apply_annotation_filter(
    doc_id: u64,
    predicate: AnnotationPredicate,
    expected_revision: u64,
    state: Db<'_>,
    annotations: State<'_, AnnotationRegistry>,
) -> AppResult<DocumentMeta> {
    let handle = doc_handle(&state, doc_id)?;
    let mut doc = handle.write().map_err(poisoned)?;
    doc.check_revision(expected_revision)?;
    let records = {
        let source = DocumentSource::new(&doc);
        annotations.try_with(doc_id, |store| {
            store.matching_records(&source, &predicate, None)
        })?
    };
    let rows: Vec<usize> = records.into_iter().map(|r| r as usize).collect();
    doc.set_filter(rows)?;
    Ok(doc.meta())
}

/// Preview copying a tag into a column (how many rows are affected, what is
/// skipped as ambiguous / orphaned, a bounded sample). Read-only; carries the
/// document revision the apply is guarded by.
#[tauri::command]
pub fn preview_tag_to_column(
    doc_id: u64,
    tag: String,
    state: Db<'_>,
    annotations: State<'_, AnnotationRegistry>,
) -> AppResult<TagToColumnPreview> {
    edit_annotations(&state, &annotations, doc_id, |doc, store| {
        store.preview_tag_to_column(&DocumentSource::new(doc), &tag, doc.revision(), None)
    })
}

/// Copy a tag into a real column as ONE undoable document operation: a fresh
/// column (filled for tagged rows, blank elsewhere) or writes into an existing
/// column (only the tagged rows). Guarded by the document revision. The notes
/// themselves are untouched — this materialises a copy, on request.
#[tauri::command]
pub fn apply_tag_to_column(
    doc_id: u64,
    tag: String,
    target: TagToColumnTarget,
    expected_revision: u64,
    state: Db<'_>,
    annotations: State<'_, AnnotationRegistry>,
) -> AppResult<DocumentMeta> {
    let handle = doc_handle(&state, doc_id)?;
    let mut doc = handle.write().map_err(poisoned)?;
    doc.check_revision(expected_revision)?;
    doc.ensure_editable()?;
    // record → tag value, computed against a read snapshot of the same doc.
    let writes = {
        let source = DocumentSource::new(&doc);
        annotations.try_with(doc_id, |store| {
            store.tag_to_column_writes(&source, &tag, None)
        })?
    };
    match target {
        TagToColumnTarget::NewColumn { name } => {
            let n_rows = doc.n_rows();
            let mut values = vec![String::new(); n_rows];
            for (record, value) in writes {
                if let Some(slot) = values.get_mut(record as usize) {
                    *slot = value;
                }
            }
            let insert_at = doc.n_cols();
            doc.replace_columns(Vec::new(), insert_at, vec![(name, values)])?;
        }
        TagToColumnTarget::ExistingColumn { column } => {
            if column >= doc.n_cols() {
                return Err(AppError::invalid("column index out of range"));
            }
            let changes: Vec<(usize, usize, String)> = writes
                .into_iter()
                .map(|(record, value)| (record as usize, column, value))
                .collect();
            doc.set_cells(changes)?;
        }
    }
    Ok(doc.meta())
}

/// Export the annotations as versioned JSON or flat CSV (atomic write via the
/// F03 pipeline). An EXPLICIT action — notes never leave through an ordinary
/// data export.
#[tauri::command]
pub fn export_annotations(
    doc_id: u64,
    path: String,
    format: AnnotationExportFormat,
    state: Db<'_>,
    annotations: State<'_, AnnotationRegistry>,
) -> AppResult<()> {
    let rendered = edit_annotations(&state, &annotations, doc_id, |doc, store| {
        store.export_as(&DocumentSource::new(doc), format, None)
    })?;
    save_mod::atomic_write(Path::new(&path), BackupPolicy::None, |file| {
        use std::io::Write;
        file.write_all(rendered.as_bytes())?;
        Ok(rendered.len() as u64)
    })?;
    Ok(())
}

/// The full annotations export envelope for `doc_id` — what the front end writes
/// into the project's `annotations` section, or a sidecar.
#[tauri::command]
pub fn annotations_get_export(
    doc_id: u64,
    annotations: State<'_, AnnotationRegistry>,
) -> AppResult<AnnotationsExport> {
    annotations.try_with(doc_id, |store| Ok(store.to_export()))
}

/// Hydrate a document's annotation store from an export envelope (from the
/// project section on project open, say). Replaces any current store.
#[tauri::command]
pub fn annotations_load_export(
    doc_id: u64,
    export: AnnotationsExport,
    state: Db<'_>,
    annotations: State<'_, AnnotationRegistry>,
) -> AppResult<AnnotationsView> {
    annotations.set(doc_id, annotations::AnnotationStore::from_export(export))?;
    edit_annotations(&state, &annotations, doc_id, |doc, store| {
        store.view(&DocumentSource::new(doc), doc.revision(), None)
    })
}

/// Load a document's annotations from its sidecar file (`<source>` →
/// `<source>.ceesvee-notes.json`), replacing any current store. An absent
/// sidecar yields an empty store. Used when no project is open.
#[tauri::command]
pub fn annotations_load_sidecar(
    doc_id: u64,
    source_path: String,
    state: Db<'_>,
    annotations: State<'_, AnnotationRegistry>,
) -> AppResult<AnnotationsView> {
    let store = annotations::load_sidecar(Path::new(&source_path))?;
    annotations.set(doc_id, store)?;
    edit_annotations(&state, &annotations, doc_id, |doc, store| {
        store.view(&DocumentSource::new(doc), doc.revision(), None)
    })
}

/// Save a document's annotations to its sidecar file (atomic). An empty store
/// deletes the sidecar. When a project is open the front end persists into the
/// project's `annotations` section instead (the project absorbs the sidecar on
/// save — the simple migration rule).
#[tauri::command]
pub fn annotations_save_sidecar(
    doc_id: u64,
    source_path: String,
    annotations: State<'_, AnnotationRegistry>,
) -> AppResult<()> {
    let store = annotations.try_with(doc_id, |store| Ok(store.clone()))?;
    annotations::save_sidecar(Path::new(&source_path), &store)
}

// ----- batch recipes (F25) ------------------------------------------------------

/// Validate a batch (recipe version, steps, templates, distinct output
/// names) without running anything.
#[tauri::command]
pub fn validate_recipe_batch(options: BatchOptions, app: tauri::AppHandle) -> AppResult<()> {
    let profiles = settings::load_settings(&settings_dir(&app)?).profiles;
    recipe::validate_batch(&options, &profiles)
}

/// Run a batch recipe as a cancellable job (kind "batch"). The structured
/// report (one entry per input file) lands in the cache under the job id.
/// A dry run performs every step but writes nothing.
#[tauri::command]
pub async fn start_recipe_batch(
    options: BatchOptions,
    app: tauri::AppHandle,
    jobs: State<'_, JobRegistry>,
    recipe_cache: State<'_, RecipeCache>,
) -> AppResult<u64> {
    let profiles = settings::load_settings(&settings_dir(&app)?).profiles;
    recipe::validate_batch(&options, &profiles)?;
    let sink = recipe_cache.share();
    let ctx = jobs.begin_for_app(&app, "batch", None);
    let job_id = ctx.id;
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let report = recipe::run_batch(&options, &profiles, ctx)?;
            if let Ok(mut map) = sink.lock() {
                map.insert(job_id, report);
            }
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

/// The report of a finished batch, by its job id.
#[tauri::command]
pub fn get_batch_report(job_id: u64, recipe_cache: State<'_, RecipeCache>) -> Option<BatchReport> {
    recipe_cache.get(job_id)
}

// ----- pivot / unpivot / transpose (F23) ---------------------------------------

/// Preview a reshape: projected dimensions, duplicate pivot coordinates,
/// and column limits. Nothing is created.
#[tauri::command]
pub async fn preview_reshape(
    doc_id: u64,
    spec: ReshapeSpec,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<ReshapePreview> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let doc = handle.read().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
        reshape::preview(&doc, &spec)
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Run a reshape as a cancellable "derive" job into a NEW document (F23).
#[tauri::command]
pub async fn start_reshape(
    doc_id: u64,
    spec: ReshapeSpec,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
) -> AppResult<IndexedOpenStart> {
    let handle = doc_handle(&state, doc_id)?;
    let new_doc_id = lock(&state)?.alloc_id();
    let cache_root = index_cache_root(&app)?;

    let ctx = jobs.begin_for_app(&app, "derive", Some(new_doc_id));
    let job_id = ctx.id;
    let app_for_job = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            use tauri::Manager;
            let source = handle.read().map_err(poisoned)?;
            source.check_revision(expected_revision)?;
            let doc = reshape::run(&source, &spec, new_doc_id, cache_root, ctx)?;
            drop(source);
            let registry = app_for_job.state::<Mutex<AppState>>();
            registry
                .lock()
                .map_err(|_| AppError::Other("internal state lock error".into()))?
                .insert(doc);
            Ok(())
        })
        .await;
    });
    Ok(IndexedOpenStart {
        job_id,
        doc_id: new_doc_id,
    })
}

// ----- relational joins (F21) --------------------------------------------------

/// Cardinality preview of a join — match/unmatched/duplicate-key counts and
/// the projected output size. Nothing is created.
#[tauri::command]
pub async fn preview_join(
    left_doc: u64,
    right_doc: u64,
    spec: JoinSpec,
    left_revision: u64,
    right_revision: u64,
    state: Db<'_>,
) -> AppResult<JoinPreview> {
    let left = doc_handle(&state, left_doc)?;
    let right = doc_handle(&state, right_doc)?;
    tauri::async_runtime::spawn_blocking(move || {
        let left = left.read().map_err(poisoned)?;
        let right = right.read().map_err(poisoned)?;
        left.check_revision(left_revision)?;
        right.check_revision(right_revision)?;
        joins::preview(&left, &right, &spec)
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Run a join as a cancellable "derive" job (F21). The NEW document
/// registers under the returned doc id; both sources stay untouched.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn start_join(
    left_doc: u64,
    right_doc: u64,
    spec: JoinSpec,
    left_revision: u64,
    right_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
) -> AppResult<IndexedOpenStart> {
    let left = doc_handle(&state, left_doc)?;
    let right = doc_handle(&state, right_doc)?;
    let doc_id = lock(&state)?.alloc_id();
    let cache_root = index_cache_root(&app)?;

    let ctx = jobs.begin_for_app(&app, "derive", Some(doc_id));
    let job_id = ctx.id;
    let app_for_job = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            use tauri::Manager;
            let left = left.read().map_err(poisoned)?;
            let right = right.read().map_err(poisoned)?;
            left.check_revision(left_revision)?;
            right.check_revision(right_revision)?;
            let doc = joins::run(&left, &right, &spec, doc_id, cache_root, ctx)?;
            drop(left);
            drop(right);
            let registry = app_for_job.state::<Mutex<AppState>>();
            registry
                .lock()
                .map_err(|_| AppError::Other("internal state lock error".into()))?
                .insert(doc);
            Ok(())
        })
        .await;
    });
    Ok(IndexedOpenStart { job_id, doc_id })
}

/// Preview a sampling/partitioning run (F48): resolves the seed (drawing a
/// crypto-random one when none is supplied), sizes the scope, and reports both
/// the projected and the exact per-output counts (plus a strata table and any
/// warnings). Read-only and revision-guarded; nothing is created.
#[tauri::command]
pub async fn preview_sample(
    doc_id: u64,
    request: SampleRequest,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<SamplePreview> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let doc = handle.read().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
        let source = DocumentSource::new(&doc);
        let universe = sampling::universe_for(&doc, request.scope);
        sampling::preview(&source, &universe, &request, doc.revision(), None)
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Run a sampling/partitioning operation as a cancellable job (F48). `seed` is
/// the value surfaced by [`preview_sample`] — pass it back for reproducibility.
/// Outputs become NEW derived documents (the returned `doc_ids`, registered
/// only once the whole job succeeds so a cancel leaves nothing behind) or CSV
/// files with an optional manifest (a cancel removes every committed file).
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn start_sample(
    doc_id: u64,
    request: SampleRequest,
    seed: u64,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
) -> AppResult<SampleStart> {
    let handle = doc_handle(&state, doc_id)?;
    let n_outputs = match &request.plan {
        SamplePlan::Sampling(_) => 1,
        SamplePlan::Partitioning(spec) => spec.parts.len(),
    };
    let derived = matches!(request.destination, SampleDestination::DerivedDocuments);
    let cache_root = index_cache_root(&app)?;

    // Reserve the output document ids up front (derived destination only).
    let doc_ids: Vec<u64> = if derived {
        let mut guard = lock(&state)?;
        (0..n_outputs).map(|_| guard.alloc_id()).collect()
    } else {
        Vec::new()
    };

    let job_doc = doc_ids.first().copied().or(Some(doc_id));
    let ctx = jobs.begin_for_app(&app, "sample", job_doc);
    let job_id = ctx.id;
    let app_for_job = app.clone();
    let doc_ids_for_job = doc_ids.clone();
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            use tauri::Manager;
            let source_doc = handle.read().map_err(poisoned)?;
            source_doc.check_revision(expected_revision)?;
            let source = DocumentSource::new(&source_doc);
            let universe = sampling::universe_for(&source_doc, request.scope);
            let total = universe.len();
            let plan = sampling::plan_for(&source, &universe, &request, seed, Some(ctx))?;
            match &request.destination {
                SampleDestination::DerivedDocuments => {
                    let (docs, _manifest) = sampling::execute_to_derived(
                        &source,
                        &plan.outputs,
                        &doc_ids_for_job,
                        cache_root,
                        seed,
                        request.scope,
                        request.order,
                        total,
                        &plan.method_label,
                        ctx,
                    )?;
                    // The new documents are registered only after the whole job
                    // succeeds, so a cancellation leaves nothing behind. The
                    // source read guard (a different document's lock) is safe to
                    // hold while taking the registry lock — no lock-order cycle.
                    let registry = app_for_job.state::<Mutex<AppState>>();
                    let mut guard = registry
                        .lock()
                        .map_err(|_| AppError::Other("internal state lock error".into()))?;
                    for doc in docs {
                        guard.insert(doc);
                    }
                    Ok(())
                }
                SampleDestination::Export {
                    dir,
                    base_name,
                    options,
                    write_manifest,
                } => {
                    sampling::execute_to_export(
                        &source,
                        &plan.outputs,
                        Path::new(dir),
                        base_name,
                        options,
                        *write_manifest,
                        seed,
                        request.scope,
                        request.order,
                        total,
                        &plan.method_label,
                        ctx,
                    )?;
                    Ok(())
                }
            }
        })
        .await;
    });
    Ok(SampleStart { job_id, doc_ids })
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
        doc.set_filter(rows)?;
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
    // Resolve the compare's documents BEFORE touching AppState: every path
    // nests the cache mutex outside the registry mutex (get_compare_results
    // does cache -> doc lookup too), so an ABBA deadlock cannot form.
    let (left_doc, right_doc) = compare_cache
        .with(compare_id, |r| (r.left_doc, r.right_doc))
        .ok_or_else(|| AppError::invalid("comparison no longer exists"))?;
    // Resolve document handles up front (the job outlives this command).
    let state_docs = lock(&state)?;
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
        doc.set_filter(rows)?;
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
            doc.clear_row_view();
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
        // Transforms mutate; the engine reads the in-memory rows directly.
        doc.ensure_editable()?;
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
        doc.ensure_editable()?;
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
            doc.clear_row_view();
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
    read_doc(&state, doc_id, |doc| doc.get_rows(start, count))
}

/// The COMPLETE content of one cell (display coordinates), read through the
/// backing-aware path so the F13 cell editor never operates on truncated
/// grid text. Works for indexed documents (inspection is read-only there).
#[tauri::command]
pub fn get_cell(doc_id: u64, row: usize, col: usize, state: Db<'_>) -> AppResult<String> {
    read_doc(&state, doc_id, |doc| {
        let abs = abs_row(doc, row)?;
        if col >= doc.n_cols() {
            return Err(AppError::invalid("cell index out of range"));
        }
        let rows = doc.fetch_rows(&[abs])?;
        Ok(rows
            .into_iter()
            .next()
            .and_then(|mut r| {
                if col < r.len() {
                    Some(r.swap_remove(col))
                } else {
                    None
                }
            })
            .unwrap_or_default())
    })
}

#[tauri::command]
pub fn selection_stats(doc_id: u64, rect: CellRect, state: Db<'_>) -> AppResult<SelectionStats> {
    read_doc(&state, doc_id, |doc| doc.selection_stats(rect))
}

#[tauri::command]
pub fn column_summaries(doc_id: u64, state: Db<'_>) -> AppResult<Vec<ColumnSummary>> {
    read_doc(&state, doc_id, |doc| doc.column_summaries())
}

// ----- cell editing ------------------------------------------------------

/// Cell edits go through F31 schema validation BEFORE the model: a strict
/// column rejects an invalid value here, an advisory column applies it and
/// records a retrievable issue.
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
        schema_ops::apply_validated_cells(doc, vec![(abs, col, value)])?;
        Ok(doc.meta())
    })
}

/// Batched cell edits; same F31 validation as [`set_cell`] (one strict
/// violation rejects the whole batch before anything applies).
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
        schema_ops::apply_validated_cells(doc, translated)?;
        Ok(doc.meta())
    })
}

/// Copy As (F14): serialize a selection into a structured clipboard format.
/// Runs on the blocking pool — a large off-screen selection reads through
/// Rust's row-visit API, never the front-end cache.
#[tauri::command]
pub async fn copy_as(
    doc_id: u64,
    rows: Option<Vec<usize>>,
    cols: Vec<usize>,
    include_headers: bool,
    format: CopyFormat,
    state: Db<'_>,
) -> AppResult<String> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let doc = handle.read().map_err(poisoned)?;
        // Display rows -> absolute rows (honours an active filter). `None`
        // means every visible row — kept off the IPC wire so a million-row
        // copy doesn't ship a million-entry array.
        let abs: Vec<usize> = match rows {
            Some(rows) => rows
                .iter()
                .map(|&d| abs_row(&doc, d))
                .collect::<AppResult<_>>()?,
            None => match doc.filter_view() {
                Some(view) => view.to_vec(),
                None => (0..doc.n_rows()).collect(),
            },
        };
        clipboard::serialize_selection(&doc, &abs, &cols, include_headers, &format)
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Paste Special preview (F14): parse the clipboard text, apply the block
/// transforms, and report exactly what would change — without mutating.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn preview_paste_special(
    doc_id: u64,
    text: String,
    options: PasteOptions,
    anchor_row: usize,
    anchor_col: usize,
    selection_rows: usize,
    selection_cols: usize,
    state: Db<'_>,
) -> AppResult<PastePreview> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let doc = handle.read().map_err(poisoned)?;
        let abs = abs_insert_row(&doc, anchor_row)?;
        let block = paste::parse_clipboard(&text)?;
        let block = paste::transform_block(block, &options, selection_rows, selection_cols);
        Ok(paste::preview(&doc, &block, &options, abs, anchor_col))
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Apply a previewed Paste Special as ONE undoable operation, guarded by the
/// preview's revision.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub fn apply_paste_special(
    doc_id: u64,
    text: String,
    options: PasteOptions,
    anchor_row: usize,
    anchor_col: usize,
    selection_rows: usize,
    selection_cols: usize,
    expected_revision: u64,
    state: Db<'_>,
) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        doc.ensure_editable()?;
        doc.check_revision(expected_revision)?;
        let abs = abs_insert_row(doc, anchor_row)?;
        let block = paste::parse_clipboard(&text)?;
        let block = paste::transform_block(block, &options, selection_rows, selection_cols);
        // Pasting can grow/reshape the grid, so it drops any active filter.
        doc.clear_row_view();
        doc.paste_special(
            abs,
            anchor_col,
            block,
            options.mode,
            options.skip_blanks,
            options.first_row_headers,
        )?;
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
        doc.clear_row_view();
        doc.paste(abs, anchor_col, block)?;
        Ok(doc.meta())
    })
}

// ----- row operations ----------------------------------------------------

#[tauri::command]
pub fn insert_rows(doc_id: u64, at: usize, count: usize, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        let abs = abs_insert_row(doc, at)?;
        doc.clear_row_view();
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
        doc.clear_row_view();
        doc.delete_rows(abs)?;
        Ok(doc.meta())
    })
}

#[tauri::command]
pub fn move_row(doc_id: u64, from: usize, to: usize, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        let from_abs = abs_row(doc, from)?;
        let to_abs = abs_row(doc, to)?;
        doc.clear_row_view();
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
        doc.clear_row_view();
        doc.insert_column(at, name)?;
        Ok(doc.meta())
    })
}

#[tauri::command]
pub fn delete_columns(doc_id: u64, indices: Vec<usize>, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        doc.clear_row_view();
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
        doc.clear_row_view();
        doc.move_column(from, to)?;
        Ok(doc.meta())
    })
}

// ----- analysis ----------------------------------------------------------

#[tauri::command]
pub fn sort(doc_id: u64, keys: Vec<SortKey>, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        // Sorting reorders all rows, invalidating a filter view; drop it.
        doc.clear_row_view();
        doc.sort(&keys)?;
        Ok(doc.meta())
    })
}

#[tauri::command]
pub fn set_header_mode(doc_id: u64, has_header: bool, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        // Re-interpreting the header row shifts all row indices; drop any filter.
        doc.ensure_editable()?;
        doc.clear_row_view();
        doc.set_header_mode(has_header)?;
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
        // An ordinary filter replaces the F19 live "only new rows" range.
        doc.set_follow_range(None);
        let view = filter_mod::matching_rows(doc, &spec)?;
        // A filter that excludes nothing isn't an active filter — avoids reporting
        // "N of N rows · filtered" for a match-all or empty spec. An active view
        // sort (F12) is preserved either way.
        if view.len() == doc.n_rows() {
            doc.clear_filter()?;
        } else {
            doc.set_filter(view)?;
        }
        Ok(doc.meta())
    })
}

/// Drop the row filter. A non-destructive view sort (F12) stays applied;
/// `reset_row_view` clears both.
#[tauri::command]
pub fn clear_filter(doc_id: u64, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        doc.clear_filter()?;
        Ok(doc.meta())
    })
}

/// Set (or clear, with empty `keys`) the non-destructive view sort (F12).
/// Orders the DISPLAY view only: source rows never move, nothing enters the
/// undo stack, and the document never becomes dirty. Works on read-only
/// (indexed / follow) documents; composes with an active filter.
#[tauri::command]
pub fn set_view_sort(doc_id: u64, keys: Vec<SortKey>, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        doc.set_view_sort(keys)?;
        Ok(doc.meta())
    })
}

/// Drop BOTH row-view ingredients (filter and view sort) in one step —
/// "Reset view", and jumps that need display == absolute coordinates.
#[tauri::command]
pub fn reset_row_view(doc_id: u64, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        doc.clear_row_view();
        Ok(doc.meta())
    })
}

// ----- history -----------------------------------------------------------

#[tauri::command]
pub fn undo(doc_id: u64, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        // Undo/redo may reinstate rows the filter view doesn't account for, so the
        // view is dropped to keep coordinates consistent.
        doc.clear_row_view();
        doc.undo()?;
        Ok(doc.meta())
    })
}

#[tauri::command]
pub fn redo(doc_id: u64, state: Db<'_>) -> AppResult<DocumentMeta> {
    write_doc(&state, doc_id, |doc| {
        doc.clear_row_view();
        doc.redo()?;
        Ok(doc.meta())
    })
}

// ----- explicit schemas and typed columns (F31) ---------------------------

/// The document's explicit schema, entry names refreshed from the CURRENT
/// headers by stable column ID. `schemaRevision` moves on schema edits; the
/// document `revision` (and the dirty flag) deliberately does not.
#[tauri::command]
pub fn get_schema(doc_id: u64, state: Db<'_>) -> AppResult<SchemaInfo> {
    read_doc(&state, doc_id, |doc| Ok(schema_ops::schema_info(doc)))
}

/// The completed inference result for a document, if a `start_infer_schema`
/// job has finished since the last fetch. Read-once (taken from the cache).
#[tauri::command]
pub fn take_inferred_schema(
    doc_id: u64,
    schema_scan_cache: State<'_, SchemaScanCache>,
) -> Option<DocumentSchema> {
    schema_scan_cache.take_infer(doc_id)
}

/// Start a schema-inference scan over every column as a cancellable job.
/// READ-ONLY: nothing is assigned until the user applies entries via
/// `set_column_schema` or an import. An editable document is scanned in full
/// (testing every candidate type per cell), so it runs through the job
/// registry for progress + cancellation like the F26 semantic scan; the
/// inferred schema is fetched with `take_inferred_schema` once the job ends.
/// Indexed documents scan a bounded leading sample.
#[tauri::command]
pub async fn start_infer_schema(
    doc_id: u64,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
    schema_scan_cache: State<'_, SchemaScanCache>,
) -> AppResult<u64> {
    let handle = doc_handle(&state, doc_id)?;
    {
        let doc = handle.read().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
    }
    let sink = schema_scan_cache.infer_sink();
    let ctx = jobs.begin_for_app(&app, "schemaScan", Some(doc_id));
    let job_id = ctx.id;
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let doc = handle.read().map_err(poisoned)?;
            doc.check_revision(expected_revision)?;
            let schema = crate::schema::infer_schema(&doc, Some(ctx))?;
            if let Ok(mut map) = sink.lock() {
                map.insert(doc_id, schema);
            }
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

/// Assign or replace ONE column's schema. The entry is validated (time zone,
/// input formats), the column ID must exist, and the display name is
/// refreshed from the header (the header is the source of truth). Schema
/// edits are metadata: not undoable, never dirty the document.
#[tauri::command]
pub fn set_column_schema(
    doc_id: u64,
    schema: ColumnSchema,
    state: Db<'_>,
) -> AppResult<SchemaInfo> {
    write_doc(&state, doc_id, |doc| {
        crate::schema::validate_column_schema(&schema)?;
        let col = schema_ops::column_index(doc, &schema.column_id)?;
        let mut schema = schema;
        schema.name = doc.headers()[col].clone();
        doc.set_column_schema(schema);
        Ok(schema_ops::schema_info(doc))
    })
}

/// Drop one column's schema entry (back to implicit text).
#[tauri::command]
pub fn remove_column_schema(
    doc_id: u64,
    column_id: String,
    state: Db<'_>,
) -> AppResult<SchemaInfo> {
    write_doc(&state, doc_id, |doc| {
        doc.remove_column_schema(&column_id);
        Ok(schema_ops::schema_info(doc))
    })
}

/// Export the schema as versioned JSON (atomic write; entries in the
/// document's current column order, names refreshed from headers).
#[tauri::command]
pub async fn export_schema(doc_id: u64, path: String, state: Db<'_>) -> AppResult<()> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let json = {
            let doc = handle.read().map_err(poisoned)?;
            let info = schema_ops::schema_info(&doc);
            crate::schema::export_to_json(&info.schema, doc.column_ids())?
        };
        save_mod::atomic_write(Path::new(&path), BackupPolicy::None, |file| {
            use std::io::Write;
            file.write_all(json.as_bytes())?;
            Ok(json.len() as u64)
        })?;
        Ok(())
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Import a versioned schema JSON file, REPLACING the document's schema.
/// Entries whose column ID does not exist here are skipped and reported.
#[tauri::command]
pub async fn import_schema(
    doc_id: u64,
    path: String,
    state: Db<'_>,
) -> AppResult<SchemaImportOutcome> {
    let handle = doc_handle(&state, doc_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        let json = std::fs::read_to_string(&path)?;
        let imported = crate::schema::import_from_json(&json)?;
        let mut doc = handle.write().map_err(poisoned)?;
        schema_ops::import_into(&mut doc, imported)
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

/// Pure pre-check for the cell editor: how the declared schema judges a
/// proposed value (strict paths block on it BEFORE calling `set_cell`).
/// Records nothing — the apply path is the single recorder of issues.
#[tauri::command]
pub fn validate_cell_edit(
    doc_id: u64,
    col: usize,
    value: String,
    state: Db<'_>,
) -> AppResult<CellEditValidation> {
    read_doc(&state, doc_id, |doc| {
        Ok(schema_ops::check_edit(doc, col, &value))
    })
}

/// The advisory-validation issues recorded on the document (bounded,
/// oldest first).
#[tauri::command]
pub fn get_schema_issues(doc_id: u64, state: Db<'_>) -> AppResult<Vec<SchemaIssue>> {
    read_doc(&state, doc_id, |doc| Ok(doc.schema_issues().to_vec()))
}

#[tauri::command]
pub fn clear_schema_issues(doc_id: u64, state: Db<'_>) -> AppResult<()> {
    write_doc(&state, doc_id, |doc| {
        doc.clear_schema_issues();
        Ok(())
    })
}

/// The completed invalid-value scan for a document, if a
/// `start_schema_invalid_samples` job has finished since the last fetch.
/// Read-once (taken from the cache).
#[tauri::command]
pub fn take_schema_invalid_samples(
    doc_id: u64,
    schema_scan_cache: State<'_, SchemaScanCache>,
) -> Option<InvalidSampleReport> {
    schema_scan_cache.take_invalid(doc_id)
}

/// Start a bounded invalid-value scan of one column under its declared type
/// (exact five-state counts + samples) as a cancellable job. A full-column
/// scan of an editable document is unbounded, so — like `start_column_profile`
/// — it runs through the job registry for progress + cancellation; the report
/// is fetched with `take_schema_invalid_samples` once the job ends.
/// Data-revision-guarded up front so a stale panel request fails cleanly.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn start_schema_invalid_samples(
    doc_id: u64,
    column_id: String,
    max_samples: usize,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
    schema_scan_cache: State<'_, SchemaScanCache>,
) -> AppResult<u64> {
    let handle = doc_handle(&state, doc_id)?;
    {
        let doc = handle.read().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
        // Fail fast on a missing/undeclared column before spawning a job.
        schema_ops::column_index(&doc, &column_id)?;
    }
    let sink = schema_scan_cache.invalid_sink();
    let ctx = jobs.begin_for_app(&app, "schemaScan", Some(doc_id));
    let job_id = ctx.id;
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let doc = handle.read().map_err(poisoned)?;
            doc.check_revision(expected_revision)?;
            let report = schema_ops::invalid_samples(&doc, &column_id, max_samples, Some(ctx))?;
            if let Ok(mut map) = sink.lock() {
                map.insert(doc_id, report);
            }
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

/// The completed conversion preview for a document, if a
/// `start_convert_column_preview` job has finished since the last fetch.
/// Read-once (taken from the cache).
#[tauri::command]
pub fn take_convert_column_preview(
    doc_id: u64,
    schema_scan_cache: State<'_, SchemaScanCache>,
) -> Option<ConvertPreview> {
    schema_scan_cache.take_preview(doc_id)
}

/// Start a conversion preview of one column WITHOUT mutating (five-state
/// counts, how many cells would change, before/after samples, the invalid
/// cells that would keep their text) as a cancellable job — the same
/// full-column scan `convert_column_apply` performs, so it gets the same
/// progress + cancellation. Fetch the preview with `take_convert_column_preview`
/// once the job ends and hand its `revision` + `schemaRevision` back to
/// `convert_column_apply`.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn start_convert_column_preview(
    doc_id: u64,
    column_id: String,
    max_samples: usize,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
    schema_scan_cache: State<'_, SchemaScanCache>,
) -> AppResult<u64> {
    let handle = doc_handle(&state, doc_id)?;
    {
        let doc = handle.read().map_err(poisoned)?;
        doc.ensure_editable()?;
        doc.check_revision(expected_revision)?;
        schema_ops::column_index(&doc, &column_id)?;
    }
    let sink = schema_scan_cache.preview_sink();
    let ctx = jobs.begin_for_app(&app, "schemaScan", Some(doc_id));
    let job_id = ctx.id;
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let doc = handle.read().map_err(poisoned)?;
            doc.check_revision(expected_revision)?;
            let preview = schema_ops::convert_preview(&doc, &column_id, max_samples, Some(ctx))?;
            if let Ok(mut map) = sink.lock() {
                map.insert(doc_id, preview);
            }
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

/// Apply a previewed canonical conversion as ONE undoable operation, as a
/// job (progress + cancellation; the commit is guarded against BOTH the
/// preview's data revision and its schema revision). Invalid cells keep their
/// original text — the preview already reported their count.
#[tauri::command]
pub async fn convert_column_apply(
    doc_id: u64,
    column_id: String,
    expected_revision: u64,
    expected_schema_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
) -> AppResult<u64> {
    let handle = doc_handle(&state, doc_id)?;
    {
        let doc = handle.read().map_err(poisoned)?;
        doc.ensure_editable()?;
        doc.check_revision(expected_revision)?;
        doc.check_schema_revision(expected_schema_revision)?;
    }
    let ctx = jobs.begin_for_app(&app, "schemaConvert", Some(doc_id));
    let job_id = ctx.id;
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let mut doc = handle.write().map_err(poisoned)?;
            // Editable documents are in-memory; the scan+commit runs under
            // one write lock so the change list can never go stale between
            // computing and committing.
            schema_ops::apply_conversion(
                &mut doc,
                &column_id,
                expected_revision,
                expected_schema_revision,
                Some(ctx),
            )?;
            Ok(())
        })
        .await;
    });
    Ok(job_id)
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
    include_headers: Option<bool>,
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
        // Skipped headers are never written, so they must not block the
        // export (default: included, matching the export options default).
        if include_headers.unwrap_or(true) && doc.has_header_row() {
            let headers = doc.headers();
            for &col in &resolved.cols {
                if encoding::has_unmappable(&headers[col], target) {
                    record(None, col, &headers[col]);
                }
            }
        }
        doc.visit_rows_at(&resolved.rows, &mut |r, row| {
            for &c in &resolved.cols {
                if encoding::has_unmappable(&row[c], target) {
                    record(Some(r), c, &row[c]);
                }
            }
            Ok(true)
        })?;
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
        // Saving an indexed document in place is meaningless (its content IS
        // the file) and Save As would desync the index from `doc.path`; use
        // Export, or convert to editable first.
        doc.ensure_editable()?;
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
                    if archive::is_gzip_path(&dest) {
                        // F17: a *.gz destination streams through gzip inside
                        // the same atomic pipeline.
                        let mut encoder =
                            flate2::write::GzEncoder::new(file, flate2::Compression::default());
                        let bytes =
                            export::write_document(&doc, &options, &mut encoder, Some(ctx))?;
                        encoder.finish()?;
                        Ok(bytes)
                    } else {
                        export::write_document(&doc, &options, file, Some(ctx))
                    }
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
                // The crash-recovery journal restarts against the saved
                // baseline (F16) — its compaction step.
                doc.reset_journal_baseline();
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

// ----- JSON / JSON Lines interoperability (F33) --------------------------------

/// Start a full-pass JSON import preview scan as a cancellable job (kind
/// "scan"): detected shape + pointer candidates, the deterministic key
/// union, per-column present/null/missing counts, nested-object and
/// array-field reports, projected dimensions and bounded sample rows.
/// Nothing is created; fetch the result with `get_json_import_preview`
/// after the `job-finished` event.
#[tauri::command]
pub async fn json_import_preview(
    path: String,
    options: Option<JsonImportOptions>,
    app: tauri::AppHandle,
    jobs: State<'_, JobRegistry>,
    json_previews: State<'_, JsonImportPreviewCache>,
) -> AppResult<u64> {
    let options = options.unwrap_or_default();
    let sink = json_previews.share();
    let ctx = jobs.begin_for_app(&app, "scan", None);
    let job_id = ctx.id;
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let scan = crate::json_import::scan(Path::new(&path), &options, Some(ctx))?;
            if let Ok(mut map) = sink.lock() {
                map.insert(job_id, scan.preview);
            }
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}

/// The preview of a finished JSON import scan, by its job id.
#[tauri::command]
pub fn get_json_import_preview(
    job_id: u64,
    json_previews: State<'_, JsonImportPreviewCache>,
) -> Option<JsonImportPreview> {
    json_previews.get(job_id)
}

/// Run a JSON / JSON Lines import as a cancellable job (kind "derive"): the
/// engine re-validates the WHOLE input first, then the NEW document
/// registers under the returned doc id when the job finishes — through the
/// same derived-document pipeline as every other producer (dirty tracking,
/// tabs, spill-to-indexed for large results). Invalid input fails the job
/// and leaves no document and no stray cache files behind.
#[tauri::command]
pub async fn json_import_apply(
    path: String,
    options: Option<JsonImportOptions>,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
) -> AppResult<IndexedOpenStart> {
    let options = options.unwrap_or_default();
    let doc_id = lock(&state)?.alloc_id();
    let cache_root = index_cache_root(&app)?;

    let ctx = jobs.begin_for_app(&app, "derive", Some(doc_id));
    let job_id = ctx.id;
    let app_for_job = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            use tauri::Manager;
            let doc = crate::json_import::import(
                Path::new(&path),
                &options,
                &cache_root,
                doc_id,
                Some(ctx),
            )?;
            let registry = app_for_job.state::<Mutex<AppState>>();
            registry
                .lock()
                .map_err(|_| AppError::Other("internal state lock error".into()))?
                .insert(doc);
            Ok(())
        })
        .await;
    });
    Ok(IndexedOpenStart { job_id, doc_id })
}

/// Start a scoped JSON / JSON Lines export as a cancellable job (kind
/// "export"): objects, arrays or JSON Lines through the atomic-save
/// pipeline. Options validate, the scope resolves and duplicate output
/// paths are rejected BEFORE the job spawns (and re-checked inside it,
/// revision-guarded); failure or cancellation removes the staging file and
/// never touches an existing destination.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn json_export(
    doc_id: u64,
    path: String,
    options: JsonExportOptions,
    scope: ExportScope,
    expected_revision: u64,
    app: tauri::AppHandle,
    state: Db<'_>,
    jobs: State<'_, JobRegistry>,
) -> AppResult<u64> {
    let handle = doc_handle(&state, doc_id)?;
    {
        // Fail fast: stale snapshots, invalid option combinations and
        // duplicate output paths reject the invoke, not a background job.
        let doc = handle.read().map_err(poisoned)?;
        doc.check_revision(expected_revision)?;
        json_export::plan(&doc, &options, &scope)?;
    }

    let ctx = jobs.begin_for_app(&app, "export", Some(doc_id));
    let job_id = ctx.id;
    tauri::async_runtime::spawn(async move {
        let _ = crate::job::run_blocking(ctx, move |ctx| {
            let doc = handle.read().map_err(poisoned)?;
            json_export::run(
                &doc,
                Path::new(&path),
                &options,
                &scope,
                expected_revision,
                ctx,
            )?;
            Ok(())
        })
        .await;
    });
    Ok(job_id)
}
