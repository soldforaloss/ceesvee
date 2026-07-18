//! Shared infrastructure for long-running background jobs: identifiers,
//! cooperative cancellation and throttled progress events.
//!
//! A command that starts a long scan/export registers a job with
//! [`JobRegistry::begin`], moves the returned [`JobCtx`] into the worker, and
//! streams progress through it. The front end listens on the `job-progress` /
//! `job-finished` events and can abort any job with the `cancel_job` command.
//! Workers observe cancellation cooperatively via [`JobCtx::advance`] /
//! [`JobCtx::check`], which fail with [`AppError::Cancelled`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::{AppError, AppResult};

/// Event channel carrying incremental progress for a running job.
pub const PROGRESS_EVENT: &str = "job-progress";
/// Event channel carrying the terminal state of a job.
pub const FINISHED_EVENT: &str = "job-finished";

/// Progress snapshots are throttled to this interval so a tight scan loop
/// cannot flood the IPC bridge.
const MIN_EMIT_INTERVAL: Duration = Duration::from_millis(100);

/// Incremental progress payload for the `job-progress` event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobProgress {
    pub job_id: u64,
    pub doc_id: Option<u64>,
    pub kind: String,
    /// Units processed so far (rows, bytes, … — whatever the job counts).
    pub processed: u64,
    /// Total units, when known up front.
    pub total: Option<u64>,
    /// Bytes written so far, for jobs that produce output files.
    pub bytes_written: Option<u64>,
    /// Current output part (1-based), for jobs that write multiple files.
    pub part: Option<u32>,
    /// Optional human-readable stage description.
    pub message: Option<String>,
}

/// Terminal status of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum JobStatus {
    Done,
    Cancelled,
    Failed,
}

/// Terminal payload for the `job-finished` event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobFinished {
    pub job_id: u64,
    pub doc_id: Option<u64>,
    pub kind: String,
    pub status: JobStatus,
    pub error: Option<String>,
}

/// An event flowing out of a job, routed to the front end (or captured by
/// tests) through the emitter closure passed to [`JobRegistry::begin`].
#[derive(Debug, Clone)]
pub enum JobEvent {
    Progress(JobProgress),
    Finished(JobFinished),
}

type SharedFlags = Arc<Mutex<HashMap<u64, Arc<AtomicBool>>>>;

/// Process-wide registry of running jobs, managed by Tauri. Interior mutability
/// only: commands take it as `State<JobRegistry>` without an outer lock.
#[derive(Default)]
pub struct JobRegistry {
    /// Cancellation flag for every currently running job.
    flags: SharedFlags,
    next_id: AtomicU64,
}

impl JobRegistry {
    /// Register a new job and build its worker-side context. `emit` receives
    /// every progress/finished event (in production: forwards to the webview).
    pub fn begin(
        &self,
        kind: &'static str,
        doc_id: Option<u64>,
        emit: impl Fn(JobEvent) + Send + Sync + 'static,
    ) -> JobCtx {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let cancel = Arc::new(AtomicBool::new(false));
        if let Ok(mut flags) = self.flags.lock() {
            flags.insert(id, Arc::clone(&cancel));
        }
        JobCtx {
            id,
            kind,
            doc_id,
            cancel,
            registry: Arc::clone(&self.flags),
            emit: Box::new(emit),
            progress: Mutex::new(ProgressState::default()),
            finished: AtomicBool::new(false),
        }
    }

    /// Register a new job whose events are forwarded to the main window.
    pub fn begin_for_app(
        &self,
        app: &tauri::AppHandle,
        kind: &'static str,
        doc_id: Option<u64>,
    ) -> JobCtx {
        use tauri::Emitter;
        let app = app.clone();
        self.begin(kind, doc_id, move |event| match event {
            JobEvent::Progress(p) => {
                let _ = app.emit(PROGRESS_EVENT, &p);
            }
            JobEvent::Finished(f) => {
                let _ = app.emit(FINISHED_EVENT, &f);
            }
        })
    }

    /// Request cancellation of a running job. Returns whether such a job
    /// existed; cancelling an unknown (already finished) id is a no-op.
    pub fn cancel(&self, job_id: u64) -> bool {
        match self.flags.lock() {
            Ok(flags) => match flags.get(&job_id) {
                Some(flag) => {
                    flag.store(true, Ordering::Relaxed);
                    true
                }
                None => false,
            },
            Err(_) => false,
        }
    }
}

#[derive(Default)]
struct ProgressState {
    processed: u64,
    total: Option<u64>,
    bytes_written: Option<u64>,
    part: Option<u32>,
    message: Option<String>,
    last_emit: Option<Instant>,
}

/// Worker-side handle for one job: progress reporting plus cooperative
/// cancellation. Dropping the context deregisters the job.
pub struct JobCtx {
    pub id: u64,
    kind: &'static str,
    doc_id: Option<u64>,
    cancel: Arc<AtomicBool>,
    registry: SharedFlags,
    emit: Box<dyn Fn(JobEvent) + Send + Sync>,
    progress: Mutex<ProgressState>,
    finished: AtomicBool,
}

/// A cheap, `'static` handle onto one job's cancellation flag, for callbacks
/// that cannot borrow the [`JobCtx`] (the F35 SQLite progress handler is
/// installed with a `'static` closure that outlives any borrow).
#[derive(Clone)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// A token that can never be cancelled (for callers without a job).
    pub fn never() -> CancelToken {
        CancelToken(Arc::new(AtomicBool::new(false)))
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

impl JobCtx {
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// A `'static` view of this job's cancellation flag (see [`CancelToken`]).
    pub fn cancel_token(&self) -> CancelToken {
        CancelToken(Arc::clone(&self.cancel))
    }

    /// Fail with [`AppError::Cancelled`] if cancellation was requested.
    pub fn check(&self) -> AppResult<()> {
        if self.is_cancelled() {
            Err(AppError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Set (or update) the known total and emit a progress snapshot right away
    /// so the UI can size its progress bar.
    pub fn set_total(&self, total: u64) {
        if let Ok(mut p) = self.progress.lock() {
            p.total = Some(total);
            self.emit_progress(&mut p, true);
        }
    }

    /// Set the current output part (1-based) for multi-file jobs.
    pub fn set_part(&self, part: u32) {
        if let Ok(mut p) = self.progress.lock() {
            p.part = Some(part);
        }
    }

    /// Set a human-readable stage description carried on subsequent snapshots.
    pub fn set_message(&self, message: impl Into<String>) {
        if let Ok(mut p) = self.progress.lock() {
            p.message = Some(message.into());
        }
    }

    /// Accumulate bytes written (no snapshot of its own; rides along on the
    /// next [`JobCtx::advance`]).
    pub fn add_bytes(&self, delta: u64) {
        if let Ok(mut p) = self.progress.lock() {
            p.bytes_written = Some(p.bytes_written.unwrap_or(0) + delta);
        }
    }

    /// Record `delta` processed units, emitting a throttled progress snapshot,
    /// and fail if the job was cancelled. Call this from the worker's loop.
    pub fn advance(&self, delta: u64) -> AppResult<()> {
        self.check()?;
        if let Ok(mut p) = self.progress.lock() {
            p.processed += delta;
            self.emit_progress(&mut p, false);
        }
        Ok(())
    }

    /// Emit an immediate (unthrottled) progress snapshot.
    pub fn flush_progress(&self) {
        if let Ok(mut p) = self.progress.lock() {
            self.emit_progress(&mut p, true);
        }
    }

    /// Emit the terminal event. Idempotent: only the first call emits.
    pub fn finish(&self, status: JobStatus, error: Option<String>) {
        if self.finished.swap(true, Ordering::Relaxed) {
            return;
        }
        (self.emit)(JobEvent::Finished(JobFinished {
            job_id: self.id,
            doc_id: self.doc_id,
            kind: self.kind.to_string(),
            status,
            error,
        }));
    }

    fn emit_progress(&self, p: &mut ProgressState, force: bool) {
        let due = match p.last_emit {
            None => true,
            Some(at) => at.elapsed() >= MIN_EMIT_INTERVAL,
        };
        if !force && !due {
            return;
        }
        p.last_emit = Some(Instant::now());
        (self.emit)(JobEvent::Progress(JobProgress {
            job_id: self.id,
            doc_id: self.doc_id,
            kind: self.kind.to_string(),
            processed: p.processed,
            total: p.total,
            bytes_written: p.bytes_written,
            part: p.part,
            message: p.message.clone(),
        }));
    }
}

impl Drop for JobCtx {
    fn drop(&mut self) {
        if let Ok(mut flags) = self.registry.lock() {
            flags.remove(&self.id);
        }
    }
}

/// Run `work` on the blocking pool, emit the terminal `job-finished` event
/// (derived from the result), deregister the job, and hand the result back.
pub async fn run_blocking<T, F>(ctx: JobCtx, work: F) -> AppResult<T>
where
    T: Send + 'static,
    F: FnOnce(&JobCtx) -> AppResult<T> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(move || {
        let result = work(&ctx);
        let (status, error) = match &result {
            Ok(_) => (JobStatus::Done, None),
            Err(AppError::Cancelled) => (JobStatus::Cancelled, None),
            Err(e) => (JobStatus::Failed, Some(e.to_string())),
        };
        ctx.finish(status, error);
        result
    })
    .await
    .map_err(|e| AppError::Other(format!("background task failed: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Collect every emitted event into a shared vector for assertions.
    fn collector() -> (Arc<Mutex<Vec<JobEvent>>>, impl Fn(JobEvent) + Send + Sync) {
        let events: Arc<Mutex<Vec<JobEvent>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&events);
        (events, move |e| sink.lock().unwrap().push(e))
    }

    #[test]
    fn ids_are_unique_and_monotonic() {
        let registry = JobRegistry::default();
        let a = registry.begin("test", None, |_| {});
        let b = registry.begin("test", None, |_| {});
        assert!(b.id > a.id);
    }

    #[test]
    fn cancel_flags_running_job_and_advance_fails() {
        let registry = JobRegistry::default();
        let ctx = registry.begin("test", Some(7), |_| {});
        assert!(!ctx.is_cancelled());
        assert!(registry.cancel(ctx.id));
        assert!(ctx.is_cancelled());
        assert!(matches!(ctx.advance(1), Err(AppError::Cancelled)));
        assert!(matches!(ctx.check(), Err(AppError::Cancelled)));
    }

    #[test]
    fn cancel_unknown_job_is_noop() {
        let registry = JobRegistry::default();
        assert!(!registry.cancel(42));
    }

    #[test]
    fn drop_deregisters_job() {
        let registry = JobRegistry::default();
        let ctx = registry.begin("test", None, |_| {});
        let id = ctx.id;
        drop(ctx);
        assert!(!registry.cancel(id), "dropped job should be gone");
    }

    #[test]
    fn set_total_emits_snapshot_with_fields() {
        let registry = JobRegistry::default();
        let (events, sink) = collector();
        let ctx = registry.begin("scan", Some(3), sink);
        ctx.set_part(2);
        ctx.add_bytes(10);
        ctx.set_total(100);
        let events = events.lock().unwrap();
        let JobEvent::Progress(p) = &events[events.len() - 1] else {
            panic!("expected progress event");
        };
        assert_eq!(p.total, Some(100));
        assert_eq!(p.doc_id, Some(3));
        assert_eq!(p.kind, "scan");
        assert_eq!(p.part, Some(2));
        assert_eq!(p.bytes_written, Some(10));
    }

    #[test]
    fn first_advance_emits_then_throttles() {
        let registry = JobRegistry::default();
        let (events, sink) = collector();
        let ctx = registry.begin("scan", None, sink);
        ctx.advance(1).unwrap();
        let after_first = events.lock().unwrap().len();
        assert_eq!(after_first, 1, "first snapshot is immediate");
        // Subsequent advances inside the throttle window do not emit…
        ctx.advance(1).unwrap();
        ctx.advance(1).unwrap();
        assert_eq!(events.lock().unwrap().len(), 1);
        // …but the counter still accumulated, visible on a forced flush.
        ctx.flush_progress();
        let events = events.lock().unwrap();
        let JobEvent::Progress(p) = &events[events.len() - 1] else {
            panic!("expected progress event");
        };
        assert_eq!(p.processed, 3);
    }

    #[test]
    fn finish_is_idempotent() {
        let registry = JobRegistry::default();
        let (events, sink) = collector();
        let ctx = registry.begin("scan", None, sink);
        ctx.finish(JobStatus::Done, None);
        ctx.finish(JobStatus::Failed, Some("late".into()));
        let events = events.lock().unwrap();
        assert_eq!(events.len(), 1);
        let JobEvent::Finished(f) = &events[0] else {
            panic!("expected finished event");
        };
        assert_eq!(f.status, JobStatus::Done);
    }

    #[test]
    fn run_blocking_maps_results_to_terminal_status() {
        let registry = JobRegistry::default();

        let (events, sink) = collector();
        let ctx = registry.begin("ok", None, sink);
        let out: AppResult<u32> = tauri::async_runtime::block_on(run_blocking(ctx, |_| Ok(5)));
        assert_eq!(out.unwrap(), 5);
        {
            let events = events.lock().unwrap();
            let JobEvent::Finished(f) = &events[events.len() - 1] else {
                panic!("expected finished event");
            };
            assert_eq!(f.status, JobStatus::Done);
        }

        let (events, sink) = collector();
        let ctx = registry.begin("cancelled", None, sink);
        registry.cancel(ctx.id);
        let out: AppResult<u32> =
            tauri::async_runtime::block_on(run_blocking(ctx, |ctx| ctx.advance(1).map(|_| 0)));
        assert!(matches!(out, Err(AppError::Cancelled)));
        {
            let events = events.lock().unwrap();
            let JobEvent::Finished(f) = &events[events.len() - 1] else {
                panic!("expected finished event");
            };
            assert_eq!(f.status, JobStatus::Cancelled);
        }

        let (events, sink) = collector();
        let ctx = registry.begin("failed", None, sink);
        let out: AppResult<u32> = tauri::async_runtime::block_on(run_blocking(ctx, |_| {
            Err(AppError::Other("boom".into()))
        }));
        assert!(out.is_err());
        let events = events.lock().unwrap();
        let JobEvent::Finished(f) = &events[events.len() - 1] else {
            panic!("expected finished event");
        };
        assert_eq!(f.status, JobStatus::Failed);
        assert_eq!(f.error.as_deref(), Some("boom"));
    }
}
