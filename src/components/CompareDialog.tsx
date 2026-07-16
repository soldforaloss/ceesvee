import { useCallback, useEffect, useState } from "react";

import { autoMapColumns } from "../lib/compare";
import * as api from "../lib/tauri";
import { useActiveMeta, useStore } from "../store/useStore";
import type { CompareSpec, DiffRecord, DiffStatus } from "../types";
import { ColumnsPicker } from "./ColumnsPicker";
import { Modal } from "./Modal";

const PAGE_SIZE = 100;

const STATUS_STYLES: Record<DiffStatus, string> = {
  added: "bg-emerald-100 text-emerald-700 dark:bg-emerald-500/15 dark:text-emerald-300",
  removed: "bg-red-100 text-red-700 dark:bg-red-500/15 dark:text-red-300",
  changed: "bg-amber-100 text-amber-700 dark:bg-amber-500/15 dark:text-amber-300",
  unchanged: "bg-zinc-100 text-zinc-500 dark:bg-zinc-800 dark:text-zinc-400",
  conflict: "bg-fuchsia-100 text-fuchsia-700 dark:bg-fuchsia-500/15 dark:text-fuchsia-300",
};

/**
 * CSV compare (F09): pick another open document, a mode (positional or
 * keyed), column mapping and normalizations; results are read-only, paginated
 * and revision-guarded, with per-status exports and a JSON change report.
 */
export function CompareDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const tabs = useStore((s) => s.tabs);
  const compare = useStore((s) => s.compare);
  const runCompare = useStore((s) => s.runCompare);
  const cancelCompare = useStore((s) => s.cancelCompare);
  const exportCompare = useStore((s) => s.exportCompare);
  const jumpToDocCell = useStore((s) => s.jumpToDocCell);

  const others = tabs.filter((t) => t.id !== meta?.id);
  const [rightId, setRightId] = useState<number | null>(others[0]?.id ?? null);
  const right = tabs.find((t) => t.id === rightId) ?? null;

  const [keyed, setKeyed] = useState(true);
  const [keyColumns, setKeyColumns] = useState<number[]>([0]);
  const [byName, setByName] = useState(true);
  const [trim, setTrim] = useState(true);
  const [caseInsensitive, setCaseInsensitive] = useState(false);
  const [blankEqual, setBlankEqual] = useState(true);
  const [numericEqual, setNumericEqual] = useState(true);
  const [dateEqual, setDateEqual] = useState(false);

  // Results paging.
  const [statusFilter, setStatusFilter] = useState<DiffStatus[]>([
    "added",
    "removed",
    "changed",
    "conflict",
  ]);
  const [page, setPage] = useState(0);
  const [records, setRecords] = useState<DiffRecord[]>([]);
  const [totalFiltered, setTotalFiltered] = useState(0);
  const [pageError, setPageError] = useState<string | null>(null);

  const { info, compareId, jobId } = compare;
  const running = jobId != null;
  const percent =
    running && compare.total
      ? Math.min(100, Math.round((compare.processed / compare.total) * 100))
      : null;

  const loadPage = useCallback(
    async (pageIndex: number, statuses: DiffStatus[]) => {
      if (compareId == null) return;
      try {
        const result = await api.getCompareResults(
          compareId,
          pageIndex * PAGE_SIZE,
          PAGE_SIZE,
          statuses,
        );
        setRecords(result.records);
        setTotalFiltered(result.totalFiltered);
        setPageError(null);
      } catch (e) {
        setRecords([]);
        setPageError(String(e));
      }
    },
    [compareId],
  );

  useEffect(() => {
    if (compareId != null) void loadPage(page, statusFilter);
  }, [compareId, page, statusFilter, loadPage]);

  if (!meta) return null;

  const spec: CompareSpec = {
    mode: keyed ? "keyed" : "positional",
    keyColumns: keyed ? keyColumns : [],
    columnMapping: right && byName ? autoMapColumns(meta.headers, right.headers) : [],
    trim,
    caseInsensitive,
    blankEqual,
    numericEqual,
    dateEqual,
  };

  const start = () => {
    if (rightId == null) return;
    setPage(0);
    void runCompare(rightId, spec);
  };

  const toggleStatus = (s: DiffStatus) => {
    setPage(0);
    setStatusFilter((current) =>
      current.includes(s) ? current.filter((x) => x !== s) : [...current, s],
    );
  };

  const pages = Math.max(1, Math.ceil(totalFiltered / PAGE_SIZE));

  return (
    <Modal title="Compare documents" onClose={onClose} size="xl">
      <div className="space-y-3 text-sm">
        {others.length === 0 ? (
          <p className="py-6 text-center text-xs text-zinc-400">
            Open the file to compare against in another tab first.
          </p>
        ) : (
          <>
            <div className="flex flex-wrap items-center gap-3 text-xs">
              <span className="max-w-40 truncate font-medium">{meta.fileName}</span>
              <span className="text-zinc-400">vs</span>
              <select
                value={rightId ?? undefined}
                onChange={(e) => setRightId(Number(e.target.value))}
                className={selectCls}
              >
                {others.map((t) => (
                  <option key={t.id} value={t.id} className="dark:bg-zinc-800">
                    {t.fileName}
                  </option>
                ))}
              </select>

              <div className="flex overflow-hidden rounded border border-zinc-300 dark:border-zinc-700">
                {(
                  [
                    [true, "By key"],
                    [false, "By position"],
                  ] as const
                ).map(([value, label]) => (
                  <button
                    key={label}
                    onClick={() => setKeyed(value)}
                    className={`px-2 py-1 ${
                      keyed === value
                        ? "bg-violet-600 text-white"
                        : "text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"
                    }`}
                  >
                    {label}
                  </button>
                ))}
              </div>
            </div>

            {keyed && (
              <div className="space-y-1">
                <span className="text-xs text-zinc-500 dark:text-zinc-400">Key columns</span>
                <ColumnsPicker headers={meta.headers} value={keyColumns} onChange={setKeyColumns} />
              </div>
            )}

            <div className="flex flex-wrap gap-x-5 gap-y-1.5 text-xs">
              <Check
                label="Map columns by name"
                checked={byName}
                onChange={setByName}
                title="Pairs same-named columns even when reordered; off = by position"
              />
              <Check label="Trim" checked={trim} onChange={setTrim} />
              <Check
                label="Case-insensitive"
                checked={caseInsensitive}
                onChange={setCaseInsensitive}
              />
              <Check label="Blanks equal" checked={blankEqual} onChange={setBlankEqual} />
              <Check label="1 = 1.0" checked={numericEqual} onChange={setNumericEqual} />
              <Check label="Normalize dates" checked={dateEqual} onChange={setDateEqual} />
            </div>

            <div className="flex items-center gap-3">
              <button
                onClick={start}
                disabled={running || rightId == null || (keyed && keyColumns.length === 0)}
                className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
              >
                {running ? "Comparing…" : "Compare"}
              </button>
              {running && (
                <>
                  <div className="h-1.5 w-40 overflow-hidden rounded bg-zinc-100 dark:bg-zinc-800">
                    <div
                      className="h-full rounded bg-violet-500 transition-[width]"
                      style={{ width: `${percent ?? 5}%` }}
                    />
                  </div>
                  <button
                    onClick={() => void cancelCompare()}
                    className="rounded px-1.5 py-0.5 text-xs text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10"
                  >
                    Cancel
                  </button>
                </>
              )}
            </div>

            {compare.error && (
              <p className="rounded border border-red-200 bg-red-50 px-2 py-1.5 text-xs text-red-700 dark:border-red-900/60 dark:bg-red-950/50 dark:text-red-300">
                {compare.error}
              </p>
            )}

            {info && !running && (
              <div className="space-y-2 rounded border border-zinc-200 px-3 py-2 text-xs dark:border-zinc-800">
                <div className="flex flex-wrap items-center gap-1.5">
                  {(
                    [
                      ["added", info.summary.added],
                      ["removed", info.summary.removed],
                      ["changed", info.summary.changed],
                      ["conflict", info.summary.conflicts],
                      ["unchanged", info.summary.unchanged],
                    ] as [DiffStatus, number][]
                  ).map(([status, count]) => (
                    <button
                      key={status}
                      onClick={() => toggleStatus(status)}
                      className={`rounded px-1.5 py-0.5 font-medium tabular-nums ${STATUS_STYLES[status]} ${
                        statusFilter.includes(status) ? "" : "opacity-35"
                      }`}
                      title={`Toggle ${status} rows`}
                    >
                      {count.toLocaleString()} {status}
                    </button>
                  ))}
                  <div className="flex-1" />
                  <button onClick={() => void exportCompare("added")} className={btnTiny}>
                    Export added
                  </button>
                  <button onClick={() => void exportCompare("removed")} className={btnTiny}>
                    removed
                  </button>
                  <button onClick={() => void exportCompare("changed")} className={btnTiny}>
                    changed
                  </button>
                  <button onClick={() => void exportCompare("report")} className={btnTiny}>
                    JSON report
                  </button>
                </div>

                {pageError ? (
                  <p className="rounded bg-violet-50 px-2 py-1.5 text-violet-700 dark:bg-violet-500/10 dark:text-violet-300">
                    {pageError.includes("stale")
                      ? "A document changed since this comparison — run it again."
                      : pageError}
                  </p>
                ) : (
                  <>
                    <div className="max-h-72 overflow-y-auto">
                      <table className="w-full border-collapse">
                        <thead className="sticky top-0 bg-white text-left text-[10px] uppercase tracking-wide text-zinc-400 dark:bg-zinc-900">
                          <tr>
                            <th className="py-1 pr-2 font-medium">Status</th>
                            <th className="py-1 pr-2 font-medium">Key</th>
                            <th className="py-1 pr-2 font-medium">Rows</th>
                            <th className="py-1 font-medium">Differences (old → new)</th>
                          </tr>
                        </thead>
                        <tbody>
                          {records.map((r, i) => (
                            <tr
                              key={i}
                              className="border-t border-zinc-100 align-top dark:border-zinc-800/60"
                            >
                              <td className="py-1 pr-2">
                                <span
                                  className={`rounded px-1 py-0.5 text-[10px] font-semibold ${STATUS_STYLES[r.status]}`}
                                >
                                  {r.status}
                                </span>
                              </td>
                              <td
                                className="max-w-[10rem] truncate py-1 pr-2 font-mono"
                                title={r.key.join(" · ")}
                              >
                                {r.key.join(" · ")}
                              </td>
                              <td className="whitespace-nowrap py-1 pr-2 tabular-nums text-zinc-400">
                                {r.leftRow !== null && (
                                  <button
                                    className="hover:text-violet-600 hover:underline dark:hover:text-violet-300"
                                    onClick={() =>
                                      info && void jumpToDocCell(info.leftDoc, r.leftRow ?? 0)
                                    }
                                    title="Jump to the row in the left document"
                                  >
                                    L{(r.leftRow ?? 0) + 1}
                                  </button>
                                )}
                                {r.leftRow !== null && r.rightRow !== null && " · "}
                                {r.rightRow !== null && (
                                  <button
                                    className="hover:text-violet-600 hover:underline dark:hover:text-violet-300"
                                    onClick={() =>
                                      info && void jumpToDocCell(info.rightDoc, r.rightRow ?? 0)
                                    }
                                    title="Jump to the row in the right document"
                                  >
                                    R{(r.rightRow ?? 0) + 1}
                                  </button>
                                )}
                              </td>
                              <td className="py-1">
                                {r.cells.map((c, j) => (
                                  <div key={j} className="truncate font-mono">
                                    <span className="text-zinc-400">
                                      {meta.headers[c.leftCol]?.trim() || `col ${c.leftCol + 1}`}:
                                    </span>{" "}
                                    <span className="text-red-600 line-through dark:text-red-400">
                                      {c.left || "∅"}
                                    </span>{" "}
                                    →{" "}
                                    <span className="text-emerald-700 dark:text-emerald-300">
                                      {c.right || "∅"}
                                    </span>
                                  </div>
                                ))}
                              </td>
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    </div>

                    <div className="flex items-center justify-between tabular-nums text-zinc-400">
                      <span>
                        {totalFiltered.toLocaleString()} row{totalFiltered === 1 ? "" : "s"} match
                        the filter
                      </span>
                      <span className="flex items-center gap-2">
                        <button
                          onClick={() => setPage((p) => Math.max(0, p - 1))}
                          disabled={page === 0}
                          className={btnTiny}
                        >
                          ‹ Prev
                        </button>
                        page {page + 1} / {pages}
                        <button
                          onClick={() => setPage((p) => Math.min(pages - 1, p + 1))}
                          disabled={page >= pages - 1}
                          className={btnTiny}
                        >
                          Next ›
                        </button>
                      </span>
                    </div>
                  </>
                )}
              </div>
            )}
          </>
        )}
      </div>
    </Modal>
  );
}

function Check({
  label,
  checked,
  onChange,
  title,
}: {
  label: string;
  checked: boolean;
  onChange: (v: boolean) => void;
  title?: string;
}) {
  return (
    <label className="flex cursor-pointer items-center gap-1.5 select-none" title={title}>
      <input
        type="checkbox"
        checked={checked}
        onChange={(e) => onChange(e.target.checked)}
        className="accent-violet-600"
      />
      {label}
    </label>
  );
}

const selectCls =
  "rounded border border-zinc-300 bg-transparent px-1.5 py-1 text-xs outline-none focus:border-violet-500 dark:border-zinc-700";
const btnTiny =
  "rounded border border-zinc-300 px-1.5 py-0.5 text-[11px] text-zinc-600 hover:bg-zinc-100 disabled:opacity-40 dark:border-zinc-700 dark:text-zinc-300 dark:hover:bg-zinc-800";
