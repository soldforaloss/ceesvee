import { describe, expect, it } from "vitest";

import type { DiagnosticIssue, DiagnosticsReport } from "../types";
import { isReportStale, issueCount, progressPercent, sortIssues } from "./diagnostics";

function issue(id: string, severity: DiagnosticIssue["severity"]): DiagnosticIssue {
  return {
    id,
    kind: id,
    severity,
    title: id,
    description: "",
    affectedCount: 1,
    samples: [],
    suggestedAction: null,
    rowFilterable: false,
  };
}

function report(docId: number, revision: number): DiagnosticsReport {
  return { docId, revision, source: [], current: [issue("a", "info")] };
}

describe("sortIssues", () => {
  it("orders errors before warnings before info, stably", () => {
    const sorted = sortIssues([
      issue("i1", "info"),
      issue("w1", "warning"),
      issue("e1", "error"),
      issue("w2", "warning"),
    ]);
    expect(sorted.map((i) => i.id)).toEqual(["e1", "w1", "w2", "i1"]);
  });
});

describe("isReportStale", () => {
  it("is fresh when doc and revision match", () => {
    expect(isReportStale({ id: 1, revision: 5 }, report(1, 5))).toBe(false);
  });

  it("is stale after any revision bump", () => {
    expect(isReportStale({ id: 1, revision: 6 }, report(1, 5))).toBe(true);
  });

  it("is stale when the report belongs to another document", () => {
    expect(isReportStale({ id: 2, revision: 5 }, report(1, 5))).toBe(true);
  });

  it("is not stale when either side is missing", () => {
    expect(isReportStale(null, report(1, 5))).toBe(false);
    expect(isReportStale({ id: 1, revision: 5 }, null)).toBe(false);
  });
});

describe("progressPercent", () => {
  it("computes bounded integer percentages", () => {
    expect(progressPercent(50, 200)).toBe(25);
    expect(progressPercent(999, 1000)).toBe(100);
    expect(progressPercent(2000, 1000)).toBe(100);
  });

  it("returns null while the total is unknown", () => {
    expect(progressPercent(10, null)).toBeNull();
    expect(progressPercent(10, 0)).toBeNull();
  });
});

describe("issueCount", () => {
  it("counts both sections", () => {
    const r = report(1, 1);
    r.source.push(issue("s", "warning"));
    expect(issueCount(r)).toBe(2);
    expect(issueCount(null)).toBe(0);
  });
});
