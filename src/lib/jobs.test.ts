import { describe, expect, it, vi } from "vitest";

import type { JobFinished, JobProgress } from "../types";

const listeners = new Map<string, (event: { payload: unknown }) => void>();

vi.mock("@tauri-apps/api/event", () => ({
  listen: (channel: string, handler: (event: { payload: unknown }) => void) => {
    listeners.set(channel, handler);
    return Promise.resolve(() => listeners.delete(channel));
  },
}));

import {
  isCancelledError,
  isStaleRevisionError,
  JOB_FINISHED_EVENT,
  JOB_PROGRESS_EVENT,
  onJobFinished,
  onJobProgress,
} from "./jobs";

describe("job event subscriptions", () => {
  it("delivers progress payloads from the job-progress channel", async () => {
    const seen: JobProgress[] = [];
    const unlisten = await onJobProgress((p) => seen.push(p));

    const payload: JobProgress = {
      jobId: 3,
      docId: 1,
      kind: "diagnostics",
      processed: 500,
      total: 1000,
      bytesWritten: null,
      part: null,
      message: null,
    };
    listeners.get(JOB_PROGRESS_EVENT)?.({ payload });

    expect(seen).toEqual([payload]);
    unlisten();
    expect(listeners.has(JOB_PROGRESS_EVENT)).toBe(false);
  });

  it("delivers terminal payloads from the job-finished channel", async () => {
    const seen: JobFinished[] = [];
    const unlisten = await onJobFinished((f) => seen.push(f));

    const payload: JobFinished = {
      jobId: 3,
      docId: 1,
      kind: "diagnostics",
      status: "cancelled",
      error: null,
    };
    listeners.get(JOB_FINISHED_EVENT)?.({ payload });

    expect(seen).toEqual([payload]);
    unlisten();
  });
});

describe("error classifiers", () => {
  it("recognises stale-revision rejections", () => {
    expect(
      isStaleRevisionError(
        "stale revision: the document changed since this operation was prepared (expected revision 4, document is at 7)",
      ),
    ).toBe(true);
    expect(isStaleRevisionError(new Error("stale revision: …"))).toBe(true);
    expect(isStaleRevisionError("I/O error: permission denied")).toBe(false);
  });

  it("recognises cancellation rejections", () => {
    expect(isCancelledError("operation cancelled")).toBe(true);
    expect(isCancelledError("CSV error: bad record")).toBe(false);
  });
});
