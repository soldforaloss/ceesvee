import { useEffect } from "react";

import { isReportStale, progressPercent, sortIssues } from "../lib/diagnostics";
import { useActiveMeta, useStore } from "../store/useStore";
import type { DiagnosticIssue, DiagnosticSeverity } from "../types";
import { Close, Refresh } from "./Icons";

const SEVERITY_BADGE: Record<DiagnosticSeverity, string> = {
  error: "bg-red-100 text-red-700 dark:bg-red-500/15 dark:text-red-300",
  warning: "bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-300",
  info: "bg-sky-100 text-sky-700 dark:bg-sky-500/15 dark:text-sky-300",
};

const SHOWN_SAMPLES = 6;

export function DiagnosticsPanel() {
  const meta = useActiveMeta();
  const docState = useStore((s) => (meta ? s.diagnostics[meta.id] : undefined));
  const setOpen = useStore((s) => s.setDiagnosticsOpen);
  const runScan = useStore((s) => s.runDiagnosticsScan);
  const cancelScan = useStore((s) => s.cancelDiagnosticsScan);

  const report = docState?.report ?? null;
  const scanning = docState?.jobId != null;
  const stale = isReportStale(meta, report);

  // Scan automatically when the panel is shown for a document that has no
  // usable report yet. Rescans after edits stay manual (the stale banner).
  useEffect(() => {
    if (meta && !report && !scanning && !docState?.scanError) void runScan();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [meta?.id, report, scanning, docState?.scanError]);

  if (!meta) return null;

  const percent = progressPercent(docState?.processed ?? 0, docState?.total ?? null);

  return (
    <aside className="flex w-96 shrink-0 flex-col border-l border-zinc-200 bg-white text-sm dark:border-zinc-800 dark:bg-zinc-950">
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-zinc-200 px-3 dark:border-zinc-800">
        <span className="font-semibold text-zinc-700 dark:text-zinc-200">Diagnostics</span>
        <div className="flex-1" />
        <button
          title="Rescan"
          onClick={() => void runScan()}
          disabled={scanning}
          className="rounded p-1 text-zinc-500 hover:bg-zinc-100 disabled:opacity-30 dark:hover:bg-zinc-800"
        >
          <Refresh className="h-4 w-4" />
        </button>
        <button
          title="Close diagnostics"
          onClick={() => setOpen(false)}
          className="rounded p-1 text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"
        >
          <Close className="h-4 w-4" />
        </button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-3">
        {scanning && (
          <div className="mb-3 rounded border border-zinc-200 px-3 py-2 dark:border-zinc-800">
            <div className="mb-1.5 flex items-center justify-between text-xs">
              <span className="text-zinc-500 dark:text-zinc-400">
                Scanning{percent !== null ? ` — ${percent}%` : "…"}
              </span>
              <button
                onClick={() => void cancelScan()}
                className="rounded px-1.5 py-0.5 text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10"
              >
                Cancel
              </button>
            </div>
            <div className="h-1.5 overflow-hidden rounded bg-zinc-100 dark:bg-zinc-800">
              <div
                className="h-full rounded bg-violet-500 transition-[width]"
                style={{ width: `${percent ?? 5}%` }}
              />
            </div>
          </div>
        )}

        {docState?.scanError && !scanning && (
          <div className="mb-3 rounded border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700 dark:border-red-900/60 dark:bg-red-950/50 dark:text-red-300">
            Scan failed: {docState.scanError}
            <button
              onClick={() => void runScan()}
              className="ml-2 rounded px-1.5 py-0.5 font-medium underline"
            >
              Retry
            </button>
          </div>
        )}

        {stale && !scanning && (
          <div className="mb-3 flex items-center justify-between gap-2 rounded bg-violet-50 px-3 py-2 text-xs text-violet-700 dark:bg-violet-500/10 dark:text-violet-300">
            <span>The data changed since this scan.</span>
            <button
              onClick={() => void runScan()}
              className="shrink-0 rounded bg-violet-600 px-2 py-1 font-medium text-white hover:bg-violet-500"
            >
              Rescan
            </button>
          </div>
        )}

        {report && (
          <>
            <Section
              title="Source file"
              issues={report.source}
              emptyLabel="No import problems detected."
              stale={stale}
              headers={meta.headers}
            />
            <Section
              title="Current data"
              issues={report.current}
              emptyLabel="No data-quality issues detected."
              stale={stale}
              headers={meta.headers}
            />
          </>
        )}

        {!report && !scanning && !docState?.scanError && (
          <p className="py-8 text-center text-zinc-400">Preparing scan…</p>
        )}
      </div>
    </aside>
  );
}

function Section({
  title,
  issues,
  emptyLabel,
  stale,
  headers,
}: {
  title: string;
  issues: DiagnosticIssue[];
  emptyLabel: string;
  stale: boolean;
  headers: string[];
}) {
  return (
    <div className="mb-4">
      <div className="mb-1.5 text-[11px] font-semibold uppercase tracking-wider text-zinc-400 dark:text-zinc-500">
        {title}
      </div>
      {issues.length === 0 ? (
        <p className="text-xs text-zinc-400 dark:text-zinc-500">{emptyLabel}</p>
      ) : (
        <div className="space-y-2">
          {sortIssues(issues).map((issue) => (
            <IssueCard key={issue.id} issue={issue} stale={stale} headers={headers} />
          ))}
        </div>
      )}
    </div>
  );
}

function IssueCard({
  issue,
  stale,
  headers,
}: {
  issue: DiagnosticIssue;
  stale: boolean;
  headers: string[];
}) {
  const applyIssueFilter = useStore((s) => s.applyIssueFilter);
  const shown = issue.samples.slice(0, SHOWN_SAMPLES);
  const hidden = issue.samples.length - shown.length;

  return (
    <div className="rounded-lg border border-zinc-200 px-3 py-2 dark:border-zinc-800">
      <div className="flex items-start gap-2">
        <span
          className={`mt-0.5 shrink-0 rounded px-1.5 py-0.5 text-[10px] font-semibold uppercase ${SEVERITY_BADGE[issue.severity]}`}
        >
          {issue.severity}
        </span>
        <div className="min-w-0">
          <div className="font-medium text-zinc-800 dark:text-zinc-100">{issue.title}</div>
          <div className="text-xs tabular-nums text-zinc-400">
            {issue.affectedCount.toLocaleString()} affected
          </div>
        </div>
      </div>

      <p className="mt-1.5 text-xs leading-relaxed text-zinc-600 dark:text-zinc-300">
        {issue.description}
      </p>
      {issue.suggestedAction && (
        <p className="mt-1 text-xs text-zinc-500 dark:text-zinc-400">→ {issue.suggestedAction}</p>
      )}

      {shown.length > 0 && (
        <ul className="mt-2 space-y-1">
          {shown.map((sample, i) => (
            <SampleRow key={i} sample={sample} stale={stale} headers={headers} />
          ))}
          {hidden > 0 && (
            <li className="text-[11px] text-zinc-400">+ {hidden.toLocaleString()} more</li>
          )}
        </ul>
      )}

      {issue.rowFilterable && (
        <button
          onClick={() => void applyIssueFilter(issue.id)}
          disabled={stale}
          title={stale ? "Rescan first — the data changed" : "Show only the affected rows"}
          className="mt-2 rounded border border-violet-300 px-2 py-1 text-xs font-medium text-violet-700 hover:bg-violet-50 disabled:opacity-40 dark:border-violet-500/40 dark:text-violet-300 dark:hover:bg-violet-500/10"
        >
          Filter to affected rows
        </button>
      )}
    </div>
  );
}

function SampleRow({
  sample,
  stale,
  headers,
}: {
  sample: import("../types").DiagnosticSample;
  stale: boolean;
  headers: string[];
}) {
  const jumpToCell = useStore((s) => s.jumpToCell);
  const jumpable = !stale && (sample.row !== null || sample.col !== null);

  const place = [
    sample.row !== null ? `row ${sample.row + 1}` : null,
    sample.col !== null ? headers[sample.col]?.trim() || `col ${sample.col + 1}` : null,
  ]
    .filter(Boolean)
    .join(" · ");

  const body = (
    <>
      {place && <span className="shrink-0 tabular-nums text-zinc-400">{place}</span>}
      {sample.value !== null && (
        <span className="truncate font-mono text-[11px] text-zinc-600 dark:text-zinc-300">
          “{sample.value}”
        </span>
      )}
      {sample.note && <span className="truncate text-zinc-500">{sample.note}</span>}
    </>
  );

  if (!jumpable) {
    return <li className="flex items-baseline gap-2 text-xs">{body}</li>;
  }
  return (
    <li>
      <button
        onClick={() => void jumpToCell(sample.row ?? 0, sample.col ?? 0)}
        title="Jump to cell"
        className="flex w-full items-baseline gap-2 rounded px-1 py-0.5 text-left text-xs hover:bg-zinc-100 dark:hover:bg-zinc-800"
      >
        {body}
      </button>
    </li>
  );
}
