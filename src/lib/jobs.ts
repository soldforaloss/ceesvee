// Front-end side of the shared background-job infrastructure: typed
// subscriptions to the progress/finished events emitted by Rust jobs, plus
// classifiers for the two structured failure modes every deferred operation
// can hit (cancellation and stale document revisions).

import { listen, type UnlistenFn } from "@tauri-apps/api/event";

import type { JobFinished, JobProgress } from "../types";

/** Event channel carrying incremental progress for running jobs. */
export const JOB_PROGRESS_EVENT = "job-progress";
/** Event channel carrying the terminal state of jobs. */
export const JOB_FINISHED_EVENT = "job-finished";

/** Subscribe to progress snapshots for all running jobs. */
export function onJobProgress(callback: (progress: JobProgress) => void): Promise<UnlistenFn> {
  return listen<JobProgress>(JOB_PROGRESS_EVENT, (event) => callback(event.payload));
}

/** Subscribe to job completion (done / cancelled / failed). */
export function onJobFinished(callback: (finished: JobFinished) => void): Promise<UnlistenFn> {
  return listen<JobFinished>(JOB_FINISHED_EVENT, (event) => callback(event.payload));
}

/**
 * Whether a rejected command failed because it was generated against an older
 * document revision (the preview/result must be discarded and regenerated).
 */
export function isStaleRevisionError(error: unknown): boolean {
  return String(error).includes("stale revision");
}

/** Whether a rejected command failed because the user cancelled the job. */
export function isCancelledError(error: unknown): boolean {
  return String(error).includes("operation cancelled");
}
