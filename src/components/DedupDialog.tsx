import { useState } from "react";

import { useActiveMeta, useStore } from "../store/useStore";
import type { DedupSpec, DuplicateKeepStrategy, ExportScope } from "../types";
import { ColumnsPicker } from "./ColumnsPicker";
import { Modal } from "./Modal";

/**
 * Duplicate finder (F07): pick key columns and normalization, scan (with
 * progress + cancel), inspect the groups, then filter / export / remove.
 * Removal is one undoable operation, guarded by the scan's revision.
 */
export function DedupDialog({
  onClose,
  onExportDuplicates,
}: {
  onClose: () => void;
  onExportDuplicates: () => void;
}) {
  const meta = useActiveMeta();
  const dedup = useStore((s) => s.dedup);
  const startScan = useStore((s) => s.startDuplicateScan);
  const cancelScan = useStore((s) => s.cancelDuplicateScan);
  const filterToDuplicates = useStore((s) => s.filterToDuplicates);
  const applyDedup = useStore((s) => s.applyDedup);

  const [keyColumns, setKeyColumns] = useState<number[]>([]);
  const [trim, setTrim] = useState(true);
  const [caseInsensitive, setCaseInsensitive] = useState(false);
  const [collapseWhitespace, setCollapseWhitespace] = useState(false);
  const [blankKeysEqual, setBlankKeysEqual] = useState(true);
  const [excludeBlankKeys, setExcludeBlankKeys] = useState(false);
  const [scopeVisible, setScopeVisible] = useState(false);
  const [keep, setKeep] = useState<DuplicateKeepStrategy>("first");
  const [working, setWorking] = useState(false);

  if (!meta) return null;

  const spec: DedupSpec = {
    keyColumns,
    trim,
    caseInsensitive,
    collapseWhitespace,
    blankKeysEqual,
    excludeBlankKeys,
  };
  const scope: ExportScope = scopeVisible ? { type: "visibleRows" } : { type: "all" };
  const { report, scanJobId } = dedup;
  const scanning = scanJobId != null;
  const percent =
    scanning && dedup.total
      ? Math.min(100, Math.round((dedup.processed / dedup.total) * 100))
      : null;
  const stale = report !== null && report.revision !== meta.revision;
  const canAct = report !== null && !stale && !scanning && !working;

  const doFilter = async () => {
    await filterToDuplicates(spec, scope);
    onClose();
  };

  const doExport = async () => {
    await filterToDuplicates(spec, scope);
    onExportDuplicates();
  };

  const doRemove = async () => {
    if (!report) return;
    setWorking(true);
    const ok = await applyDedup(spec, scope, keep, report.revision);
    setWorking(false);
    if (ok) onClose();
  };

  return (
    <Modal
      title="Find duplicates"
      onClose={onClose}
      size="lg"
      footer={
        <>
          <button onClick={onClose} className={btnGhost}>
            Close
          </button>
          <button onClick={() => void doFilter()} disabled={!canAct} className={btnGhost}>
            Filter to duplicates
          </button>
          <button onClick={() => void doExport()} disabled={!canAct} className={btnGhost}>
            Export duplicates…
          </button>
          {/* Removal mutates the document, so it isn't offered for indexed
              read-only documents (F10); scan/filter/export still work. */}
          {meta.backing !== "indexedReadOnly" && (
            <button
              onClick={() => void doRemove()}
              disabled={!canAct || report?.duplicateRows === 0}
              className="rounded border border-red-300 px-3 py-1.5 text-sm text-red-700 hover:bg-red-50 disabled:opacity-40 dark:border-red-500/40 dark:text-red-300 dark:hover:bg-red-500/10"
            >
              {working
                ? "Removing…"
                : `Remove ${report?.duplicateRows.toLocaleString() ?? ""} rows`}
            </button>
          )}
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <div className="space-y-1.5">
          <span className="text-xs text-zinc-500 dark:text-zinc-400">
            Key columns (rows with equal keys are duplicates)
          </span>
          <ColumnsPicker headers={meta.headers} value={keyColumns} onChange={setKeyColumns} />
        </div>

        <div className="flex flex-wrap gap-x-5 gap-y-1.5 text-xs">
          <Check label="Trim values" checked={trim} onChange={setTrim} />
          <Check label="Case-insensitive" checked={caseInsensitive} onChange={setCaseInsensitive} />
          <Check
            label="Collapse whitespace"
            checked={collapseWhitespace}
            onChange={setCollapseWhitespace}
          />
          <Check
            label="Blank keys are equal"
            checked={blankKeysEqual}
            onChange={setBlankKeysEqual}
          />
          <Check
            label="Ignore rows with a fully blank key"
            checked={excludeBlankKeys}
            onChange={setExcludeBlankKeys}
          />
          {meta.filtered && (
            <Check label="Visible rows only" checked={scopeVisible} onChange={setScopeVisible} />
          )}
        </div>

        <div className="flex items-center gap-3">
          <button
            onClick={() => void startScan(spec, scope)}
            disabled={keyColumns.length === 0 || scanning}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            {scanning ? "Scanning…" : "Scan for duplicates"}
          </button>
          {scanning && (
            <>
              <div className="h-1.5 w-40 overflow-hidden rounded bg-zinc-100 dark:bg-zinc-800">
                <div
                  className="h-full rounded bg-violet-500 transition-[width]"
                  style={{ width: `${percent ?? 5}%` }}
                />
              </div>
              <button
                onClick={() => void cancelScan()}
                className="rounded px-1.5 py-0.5 text-xs text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10"
              >
                Cancel
              </button>
            </>
          )}
        </div>

        {dedup.error && (
          <p className="rounded border border-red-200 bg-red-50 px-2 py-1.5 text-xs text-red-700 dark:border-red-900/60 dark:bg-red-950/50 dark:text-red-300">
            {dedup.error}
          </p>
        )}

        {stale && (
          <p className="rounded bg-violet-50 px-2 py-1.5 text-xs text-violet-700 dark:bg-violet-500/10 dark:text-violet-300">
            The data changed since this scan — scan again before acting on it.
          </p>
        )}

        {report && !scanning && (
          <div className="space-y-2 rounded border border-zinc-200 px-3 py-2 text-xs dark:border-zinc-800">
            <div className="flex flex-wrap gap-x-4 gap-y-1 tabular-nums">
              <span>
                <b>{report.groupCount.toLocaleString()}</b> duplicate group
                {report.groupCount === 1 ? "" : "s"}
              </span>
              <span>
                <b>{report.duplicateRows.toLocaleString()}</b> removable row
                {report.duplicateRows === 1 ? "" : "s"}
                {report.consideredRows > 0 &&
                  ` (${((report.duplicateRows / report.consideredRows) * 100).toFixed(1)}%)`}
              </span>
              <span>
                <b>{report.remainingRows.toLocaleString()}</b> would remain
              </span>
            </div>

            {report.groupCount > 0 && (
              <>
                <div className="flex items-center gap-3">
                  <span className="text-zinc-500 dark:text-zinc-400">Keep</span>
                  {(
                    [
                      ["first", "First row"],
                      ["last", "Last row"],
                      ["mostComplete", "Most complete"],
                    ] as const
                  ).map(([value, label]) => (
                    <label key={value} className="flex cursor-pointer items-center gap-1">
                      <input
                        type="radio"
                        checked={keep === value}
                        onChange={() => setKeep(value)}
                        className="accent-violet-600"
                      />
                      {label}
                    </label>
                  ))}
                </div>

                <div className="max-h-40 overflow-y-auto">
                  <ul className="space-y-1">
                    {report.sampleGroups.map((g, i) => (
                      <li key={i} className="flex items-baseline gap-2">
                        <span className="shrink-0 rounded bg-zinc-100 px-1 tabular-nums text-zinc-500 dark:bg-zinc-800">
                          ×{g.size}
                        </span>
                        <span className="truncate font-mono">
                          {g.key.map((k) => k || "∅").join(" · ")}
                        </span>
                        <span className="shrink-0 tabular-nums text-zinc-400">
                          rows {g.rows.map((r) => r + 1).join(", ")}
                          {g.size > g.rows.length ? ", …" : ""}
                        </span>
                      </li>
                    ))}
                    {report.groupCount > report.sampleGroups.length && (
                      <li className="text-zinc-400">
                        + {(report.groupCount - report.sampleGroups.length).toLocaleString()} more
                        groups
                      </li>
                    )}
                  </ul>
                </div>
              </>
            )}
          </div>
        )}
      </div>
    </Modal>
  );
}

function Check({
  label,
  checked,
  onChange,
}: {
  label: string;
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <label className="flex cursor-pointer items-center gap-1.5 select-none">
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

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 disabled:opacity-40 dark:text-zinc-300 dark:hover:bg-zinc-800";
