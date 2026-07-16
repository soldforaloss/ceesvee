import { save as saveFileDialog } from "@tauri-apps/plugin-dialog";
import { writeTextFile } from "@tauri-apps/plugin-fs";
import { useEffect, useState } from "react";

import { actionAvailable, parseAllowedValues } from "../lib/outliers";
import * as api from "../lib/tauri";
import { useActiveMeta, useStore } from "../store/useStore";
import type { OutlierAction, OutlierActionPreview, OutlierMethod, OutlierSpec } from "../types";
import { Modal } from "./Modal";

type MethodKey = OutlierMethod["type"];

const METHOD_LABELS: Record<MethodKey, string> = {
  iqr: "Interquartile range (robust)",
  mad: "Median absolute deviation (robust)",
  zScore: "Z-score",
  percentile: "Percentile bounds",
  rareCategory: "Rare category",
  unexpectedCategory: "Unexpected values",
  patternMismatch: "Pattern mismatch",
};

const ACTION_LABELS: Record<OutlierAction, string> = {
  replaceBlank: "Replace with blank",
  replaceMedian: "Replace with median",
  capToBounds: "Cap to bounds",
  removeRows: "Remove rows",
};

/**
 * Outlier and anomaly finder (F30): statistical CANDIDATES, not verdicts.
 * Robust methods (IQR, MAD) by default, whole-column or group-wise; the
 * scan is read-only and never dirties the document. Every corrective
 * action previews exact counts and examples first and applies as one undo
 * step, revision-guarded.
 */
export function OutlierDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const outlier = useStore((s) => s.outlier);
  const startScan = useStore((s) => s.startOutlierScan);
  const cancelScan = useStore((s) => s.cancelOutlierScan);
  const loadCached = useStore((s) => s.loadCachedOutlierReport);
  const applyFilter = useStore((s) => s.applyOutlierFilter);
  const jumpToCell = useStore((s) => s.jumpToCell);
  const refresh = useStore((s) => s.refreshActiveDoc);
  const clearReport = useStore((s) => s.clearOutlierReport);

  const [column, setColumn] = useState(0);
  const [method, setMethod] = useState<MethodKey>("iqr");
  const [k, setK] = useState(1.5);
  const [threshold, setThreshold] = useState(3.5);
  const [zThreshold, setZThreshold] = useState(3);
  const [pLower, setPLower] = useState(1);
  const [pUpper, setPUpper] = useState(99);
  const [maxShare, setMaxShare] = useState(0.01);
  const [allowedText, setAllowedText] = useState("");
  const [pattern, setPattern] = useState("");
  const [groupColumns, setGroupColumns] = useState<number[]>([]);
  const [scopeVisible, setScopeVisible] = useState(false);
  const [actionPreview, setActionPreview] = useState<{
    action: OutlierAction;
    data: OutlierActionPreview;
  } | null>(null);
  const [working, setWorking] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  useEffect(() => {
    void loadCached();
  }, [loadCached]);

  const { report, spec, scanJobId, processed, total, error } = outlier;
  const scanning = scanJobId != null;

  if (!meta) return null;
  const readOnly = meta.backing === "indexedReadOnly";
  const stale = report !== null && report.revision !== meta.revision;
  const headers = meta.headers.map((h, i) => h || `Column ${i + 1}`);
  const reportMethod = spec?.method.type ?? method;

  const buildMethod = (): OutlierMethod => {
    switch (method) {
      case "iqr":
        return { type: method, k };
      case "mad":
        return { type: method, threshold };
      case "zScore":
        return { type: method, threshold: zThreshold };
      case "percentile":
        return { type: method, lower: pLower, upper: pUpper };
      case "rareCategory":
        return { type: method, maxShare };
      case "unexpectedCategory":
        return { type: method, allowed: parseAllowedValues(allowedText) };
      case "patternMismatch":
        return { type: method, pattern };
    }
  };

  const buildSpec = (): OutlierSpec => ({
    column,
    method: buildMethod(),
    groupColumns,
    scope: scopeVisible ? { type: "visibleRows" } : { type: "all" },
  });

  const runScan = () => {
    setActionPreview(null);
    setActionError(null);
    void startScan(buildSpec());
  };

  const runFilter = async () => {
    setWorking(true);
    const ok = await applyFilter();
    setWorking(false);
    if (ok) onClose();
  };

  const previewAction = async (action: OutlierAction) => {
    if (!spec || !report) return;
    setActionError(null);
    try {
      const data = await api.previewOutlierAction(meta.id, spec, action, report.revision);
      setActionPreview({ action, data });
    } catch (e) {
      setActionError(String(e));
    }
  };

  const confirmAction = async () => {
    if (!spec || !report || !actionPreview) return;
    setWorking(true);
    setActionError(null);
    try {
      await api.applyOutlierAction(meta.id, spec, actionPreview.action, report.revision);
      await refresh();
      clearReport();
      setActionPreview(null);
    } catch (e) {
      setActionError(String(e));
    } finally {
      setWorking(false);
    }
  };

  const exportReport = async () => {
    if (!report) return;
    const chosen = await saveFileDialog({
      defaultPath: "outlier-report.json",
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (typeof chosen === "string") {
      await writeTextFile(chosen, JSON.stringify(report, null, 2));
    }
  };

  const jump = async (row: number) => {
    onClose();
    await jumpToCell(row, spec?.column ?? column);
  };

  const num = (
    label: string,
    value: number,
    set: (n: number) => void,
    step = 0.1,
    min?: number,
    max?: number,
  ) => (
    <label className="flex items-center gap-1.5">
      {label}
      <input
        type="number"
        step={step}
        min={min}
        max={max}
        value={value}
        onChange={(e) => set(Number(e.target.value))}
        className="w-20 rounded border border-zinc-300 bg-transparent px-1 py-0.5 dark:border-zinc-600"
      />
    </label>
  );

  return (
    <Modal
      title="Find outliers"
      onClose={onClose}
      size="xl"
      footer={
        <>
          <span className="mr-auto text-xs text-zinc-400">
            Flags are statistical candidates, not definitive errors.
          </span>
          <button onClick={() => void exportReport()} disabled={!report} className={btnGhost}>
            Export report…
          </button>
          <button onClick={onClose} className={btnGhost}>
            Close
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <div className="flex flex-wrap items-center gap-3 text-xs">
          <label className="flex items-center gap-1.5">
            Column
            <select
              value={column}
              onChange={(e) => {
                setColumn(Number(e.target.value));
                clearReport();
              }}
              className={selectCls}
            >
              {headers.map((h, i) => (
                <option key={i} value={i} className="dark:bg-zinc-800">
                  {h}
                </option>
              ))}
            </select>
          </label>
          <label className="flex items-center gap-1.5">
            Method
            <select
              value={method}
              onChange={(e) => setMethod(e.target.value as MethodKey)}
              className={selectCls}
            >
              {Object.entries(METHOD_LABELS).map(([value, label]) => (
                <option key={value} value={value} className="dark:bg-zinc-800">
                  {label}
                </option>
              ))}
            </select>
          </label>
          {method === "iqr" && num("k", k, setK, 0.1, 0.1)}
          {method === "mad" && num("threshold", threshold, setThreshold, 0.1, 0.1)}
          {method === "zScore" && num("threshold", zThreshold, setZThreshold, 0.1, 0.1)}
          {method === "percentile" && (
            <>
              {num("lower %", pLower, setPLower, 0.5, 0, 100)}
              {num("upper %", pUpper, setPUpper, 0.5, 0, 100)}
            </>
          )}
          {method === "rareCategory" && num("max share", maxShare, setMaxShare, 0.005, 0, 1)}
          {meta.filtered && (
            <label className="flex items-center gap-1.5">
              <input
                type="checkbox"
                checked={scopeVisible}
                onChange={(e) => setScopeVisible(e.target.checked)}
                className="accent-violet-600"
              />
              Visible rows only
            </label>
          )}
        </div>

        {method === "unexpectedCategory" && (
          <label className="block text-xs">
            Allowed values (comma or newline separated)
            <textarea
              value={allowedText}
              onChange={(e) => setAllowedText(e.target.value)}
              rows={2}
              className="mt-1 w-full rounded border border-zinc-300 bg-transparent p-1.5 font-mono text-[11px] dark:border-zinc-600"
            />
          </label>
        )}
        {method === "patternMismatch" && (
          <label className="block text-xs">
            Expected pattern (regex — e.g. from a file profile rule)
            <input
              value={pattern}
              onChange={(e) => setPattern(e.target.value)}
              className="mt-1 w-full rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 font-mono dark:border-zinc-600"
            />
          </label>
        )}

        <div className="text-xs">
          <p className="mb-1 text-zinc-500 dark:text-zinc-400">
            Group by (statistics computed per group):
          </p>
          <div className="flex max-h-16 flex-wrap gap-x-3 gap-y-1 overflow-y-auto">
            {headers.map((h, i) => (
              <label key={i} className="flex items-center gap-1">
                <input
                  type="checkbox"
                  checked={groupColumns.includes(i)}
                  disabled={i === column}
                  onChange={(e) =>
                    setGroupColumns(
                      e.target.checked
                        ? [...groupColumns, i].sort((a, b) => a - b)
                        : groupColumns.filter((c) => c !== i),
                    )
                  }
                  className="accent-violet-600"
                />
                {h}
              </label>
            ))}
          </div>
        </div>

        <div className="flex items-center gap-3">
          <button
            onClick={runScan}
            disabled={scanning}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            {scanning ? "Scanning…" : "Find outliers"}
          </button>
          {scanning && (
            <>
              <span className="text-xs text-zinc-500 dark:text-zinc-400">
                {processed.toLocaleString()}
                {total != null && ` / ${total.toLocaleString()}`} rows
              </span>
              <button
                onClick={() => void cancelScan()}
                className="rounded px-2 py-1 text-xs text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10"
              >
                Cancel
              </button>
            </>
          )}
          {report && !scanning && (
            <span className="text-xs text-zinc-500 dark:text-zinc-400">
              {report.flagged.toLocaleString()} flagged of {report.considered.toLocaleString()}{" "}
              considered
              {report.blanks > 0 && ` · ${report.blanks.toLocaleString()} blank`}
              {report.invalidNumeric > 0 &&
                ` · ${report.invalidNumeric.toLocaleString()} non-numeric ignored`}
            </span>
          )}
        </div>

        {stale && (
          <p className="rounded bg-amber-50 px-2 py-1.5 text-xs text-amber-700 dark:bg-amber-500/10 dark:text-amber-300">
            The document changed since this scan — actions are disabled. Run it again.
          </p>
        )}
        {(error ?? actionError) && (
          <p className="text-xs text-red-600 dark:text-red-400">{error ?? actionError}</p>
        )}

        {report && report.flagged > 0 && !stale && (
          <div className="flex flex-wrap items-center gap-1.5 text-xs">
            <button onClick={() => void runFilter()} disabled={working} className={chipBtn}>
              Filter to candidates
            </button>
            {(Object.keys(ACTION_LABELS) as OutlierAction[])
              .filter((a) => actionAvailable(reportMethod, a))
              .map((a) => (
                <button
                  key={a}
                  onClick={() => void previewAction(a)}
                  disabled={working || readOnly}
                  title={readOnly ? "Read-only (indexed) document" : undefined}
                  className={chipBtn}
                >
                  {ACTION_LABELS[a]}…
                </button>
              ))}
          </div>
        )}

        {actionPreview && (
          <div className="rounded bg-zinc-50 p-2 text-xs dark:bg-zinc-900">
            <p className="font-medium">
              {ACTION_LABELS[actionPreview.action]} —{" "}
              {actionPreview.action === "removeRows"
                ? `${actionPreview.data.rowsRemoved.toLocaleString()} rows would be removed`
                : `${actionPreview.data.cellsAffected.toLocaleString()} cells would change`}
            </p>
            {actionPreview.data.examples.length > 0 && (
              <ul className="mt-1 space-y-0.5 font-mono text-[11px] text-zinc-500 dark:text-zinc-400">
                {actionPreview.data.examples.slice(0, 5).map((ex, i) => (
                  <li key={i} className="truncate">
                    row {ex.row + 1}: {ex.before} → {ex.after === "" ? "∅" : ex.after}
                  </li>
                ))}
              </ul>
            )}
            <div className="mt-1.5 flex gap-2">
              <button
                onClick={() => void confirmAction()}
                disabled={
                  working ||
                  (actionPreview.data.cellsAffected === 0 && actionPreview.data.rowsRemoved === 0)
                }
                className={`rounded px-2 py-1 text-white disabled:opacity-40 ${
                  actionPreview.action === "removeRows"
                    ? "bg-red-600 hover:bg-red-500"
                    : "bg-violet-600 hover:bg-violet-500"
                }`}
              >
                {working ? "Applying…" : "Apply (one undo step)"}
              </button>
              <button onClick={() => setActionPreview(null)} className={btnGhost}>
                Cancel
              </button>
            </div>
          </div>
        )}

        {report && report.groups.length > 0 && (
          <div className="max-h-[16vh] overflow-y-auto pr-1 text-xs">
            <table className="w-full text-left font-mono text-[11px]">
              <thead className="text-zinc-400">
                <tr>
                  <th className="pr-2 font-normal">group</th>
                  <th className="pr-2 font-normal">count</th>
                  <th className="pr-2 font-normal">flagged</th>
                  <th className="pr-2 font-normal">median</th>
                  <th className="pr-2 font-normal">bounds</th>
                </tr>
              </thead>
              <tbody>
                {report.groups.slice(0, 20).map((g, i) => (
                  <tr key={i} className="text-zinc-600 dark:text-zinc-300">
                    <td className="pr-2">{g.key.length === 0 ? "(all)" : g.key.join(" · ")}</td>
                    <td className="pr-2">{g.count.toLocaleString()}</td>
                    <td className="pr-2">{g.flagged.toLocaleString()}</td>
                    <td className="pr-2">{g.median ?? "—"}</td>
                    <td className="pr-2">
                      {g.lower != null && g.upper != null
                        ? `${round4(g.lower)} … ${round4(g.upper)}`
                        : "—"}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
            {report.groupsTotal > report.groups.length && (
              <p className="mt-1 text-zinc-400">
                Showing {report.groups.length} of {report.groupsTotal.toLocaleString()} groups.
              </p>
            )}
          </div>
        )}

        {report && report.sample.length > 0 && (
          <div className="max-h-[22vh] space-y-0.5 overflow-y-auto pr-1 text-xs">
            {report.sample.slice(0, 50).map((f, i) => (
              <div key={i} className="flex items-center gap-2">
                <button
                  onClick={() => void jump(f.row)}
                  title="Jump to row"
                  className="rounded border border-zinc-200 px-1 py-0 font-mono text-[11px] hover:border-violet-400 dark:border-zinc-700"
                >
                  row {f.row + 1}
                </button>
                <span className="truncate font-mono text-[11px]">{f.value}</span>
                {f.group.length > 0 && (
                  <span className="text-zinc-400">[{f.group.join(" · ")}]</span>
                )}
                <span className="truncate text-zinc-400">{f.reason}</span>
              </div>
            ))}
            {report.flagged > report.sample.length && (
              <p className="text-zinc-400">
                Showing {Math.min(50, report.sample.length)} of {report.flagged.toLocaleString()}{" "}
                flagged values.
              </p>
            )}
          </div>
        )}
      </div>
    </Modal>
  );
}

function round4(v: number): number {
  return Math.round(v * 10_000) / 10_000;
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const selectCls =
  "rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 outline-none focus:border-violet-500 dark:border-zinc-700";
const chipBtn =
  "rounded border border-zinc-200 px-1.5 py-0.5 text-[11px] hover:border-violet-400 disabled:opacity-40 dark:border-zinc-700";
