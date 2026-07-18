import { useEffect, useMemo, useState } from "react";

import {
  columnarFormatLabel,
  compressionLabel,
  defaultColumnarExportOptions,
} from "../lib/columnar";
import { scopeChoices, scopeKey } from "../lib/export";
import { formatBytes } from "../lib/save";
import * as api from "../lib/tauri";
import { useActiveMeta, useStore } from "../store/useStore";
import type {
  ColumnarCompression,
  ColumnarExportOptions,
  ColumnarFormat,
  ExportScope,
  ScopeCounts,
} from "../types";
import { Modal } from "./Modal";

const FORMATS: ColumnarFormat[] = ["parquet", "arrowFile", "arrowStream"];
const COMPRESSIONS: ColumnarCompression[] = ["uncompressed", "snappy", "zstd"];

/** Bound the per-column warning list rendered on the done phase (DOM only). */
const MAX_WARNING_ROWS = 200;

/**
 * Parquet / Arrow export (F32). Format (Parquet / Arrow IPC file = Feather v2 /
 * Arrow IPC stream), Parquet compression + row-group size, typed vs verbatim
 * emission, and any CEESVEE export scope. Typed export maps declared F31 types
 * to arrow types; cells that can't be represented export as NULL and are
 * counted — the done phase surfaces those per-column invalid-cell counts.
 * Exports never touch the document's save point.
 */
export function ColumnarExportDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const runExport = useStore((s) => s.runColumnarExport);
  const clearExport = useStore((s) => s.clearColumnarExport);
  const result = useStore((s) => s.columnarExportResult);
  const filtered = useStore((s) => s.tabs.find((t) => t.id === s.activeId)?.filtered ?? false);
  const selectedRows = useStore((s) => s.selectedRows);
  const selectedCols = useStore((s) => s.selectedCols);
  const selectionRect = useStore((s) => s.selectionPhysicalRect)();
  const viewSorted = meta?.viewSorted ?? false;

  const [opts, setOpts] = useState<ColumnarExportOptions>(() => defaultColumnarExportOptions());

  const choices = useMemo(
    () => scopeChoices(filtered, selectionRect, selectedRows, selectedCols, viewSorted),
    [filtered, selectionRect, selectedRows, selectedCols, viewSorted],
  );
  const [scopeIdx, setScopeIdx] = useState(0);
  const scope: ExportScope = (choices[scopeIdx] ?? choices[0]).scope;

  const [counts, setCounts] = useState<ScopeCounts | null>(null);

  // Clear any stale result from a previous export when the dialog opens.
  useEffect(() => {
    clearExport();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Show the expected shape for the chosen scope before anything is written.
  useEffect(() => {
    if (!meta) return;
    let stale = false;
    setCounts(null);
    api
      .exportScopeCounts(meta.id, scope)
      .then((c) => {
        if (!stale) setCounts(c);
      })
      .catch(() => undefined);
    return () => {
      stale = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [meta?.id, meta?.revision, scopeKey(scope), scopeIdx]);

  if (!meta) return null;
  const patch = (p: Partial<ColumnarExportOptions>) => setOpts((o) => ({ ...o, ...p }));

  const isParquet = opts.format === "parquet";
  const report = result.report;
  const running = result.running;

  const closeAndClear = () => {
    clearExport();
    onClose();
  };

  // ----- done phase: report + invalid-cell warning surface -------------------
  if (report) {
    const shownWarnings = report.columnWarnings.slice(0, MAX_WARNING_ROWS);
    const hiddenWarnings = report.columnWarnings.length - shownWarnings.length;
    return (
      <Modal
        title="Export complete"
        onClose={closeAndClear}
        size="lg"
        footer={
          <button onClick={closeAndClear} className={btnPrimary}>
            Done
          </button>
        }
      >
        <div className="space-y-3 text-sm">
          <dl className="grid grid-cols-2 gap-x-4 gap-y-1 rounded border border-zinc-200 px-3 py-2 text-xs dark:border-zinc-800">
            <dt className="text-zinc-500 dark:text-zinc-400">Format</dt>
            <dd>{columnarFormatLabel(report.format)}</dd>
            <dt className="text-zinc-500 dark:text-zinc-400">Rows written</dt>
            <dd className="tabular-nums">{report.rows.toLocaleString()}</dd>
            <dt className="text-zinc-500 dark:text-zinc-400">Columns</dt>
            <dd className="tabular-nums">{report.columns.toLocaleString()}</dd>
            <dt className="text-zinc-500 dark:text-zinc-400">File size</dt>
            <dd className="tabular-nums">{formatBytes(report.bytes)}</dd>
          </dl>

          {report.invalidCells > 0 ? (
            <div className="space-y-1">
              <p className="text-xs text-amber-600 dark:text-amber-400">
                {report.invalidCells.toLocaleString()} cell
                {report.invalidCells === 1 ? "" : "s"} could not be represented under the declared
                types and were written as NULL:
              </p>
              <div className="max-h-40 overflow-auto rounded border border-zinc-200 dark:border-zinc-800">
                <table className="w-full border-collapse text-[11px]">
                  <thead className="sticky top-0 bg-white text-left uppercase tracking-wide text-zinc-400 dark:bg-zinc-900">
                    <tr>
                      <th className="px-2 py-1 font-medium">Column</th>
                      <th className="px-2 py-1 text-right font-medium">Invalid cells → NULL</th>
                    </tr>
                  </thead>
                  <tbody>
                    {shownWarnings.map((w) => (
                      <tr key={w.name} className="border-t border-zinc-100 dark:border-zinc-800">
                        <td className="max-w-72 truncate px-2 py-1 font-mono" title={w.name}>
                          {w.name}
                        </td>
                        <td className="px-2 py-1 text-right tabular-nums text-amber-600 dark:text-amber-400">
                          {w.invalidCells.toLocaleString()}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
              {hiddenWarnings > 0 && (
                <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
                  + {hiddenWarnings.toLocaleString()} more column
                  {hiddenWarnings === 1 ? "" : "s"} with warnings.
                </p>
              )}
              <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
                Turn off typed export to write these columns as text verbatim instead.
              </p>
            </div>
          ) : (
            <p className="text-xs text-emerald-600 dark:text-emerald-400">
              All cells exported cleanly — no invalid values.
            </p>
          )}
        </div>
      </Modal>
    );
  }

  // ----- config phase --------------------------------------------------------
  const doExport = () => {
    if (running) return;
    void runExport(opts, scope);
  };

  return (
    <Modal
      title="Export as Parquet / Arrow"
      onClose={onClose}
      size="lg"
      footer={
        <>
          <button onClick={onClose} className={btnGhost}>
            Cancel
          </button>
          <button
            onClick={doExport}
            disabled={running}
            className={btnPrimary + " disabled:opacity-40"}
          >
            {running ? "Exporting…" : "Choose file & export"}
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <Row label="Format">
          <select
            value={opts.format}
            onChange={(e) => patch({ format: e.target.value as ColumnarFormat })}
            className={selectCls}
          >
            {FORMATS.map((f) => (
              <option key={f} value={f} className="dark:bg-zinc-800">
                {columnarFormatLabel(f)}
              </option>
            ))}
          </select>
        </Row>

        {isParquet ? (
          <>
            <Row label="Compression">
              <select
                value={opts.compression}
                onChange={(e) => patch({ compression: e.target.value as ColumnarCompression })}
                className={selectCls}
              >
                {COMPRESSIONS.map((c) => (
                  <option key={c} value={c} className="dark:bg-zinc-800">
                    {compressionLabel(c)}
                  </option>
                ))}
              </select>
            </Row>
            <Row label="Rows per row group">
              <input
                type="number"
                min={0}
                value={opts.rowGroupRows}
                onChange={(e) => patch({ rowGroupRows: Math.max(0, Number(e.target.value) || 0) })}
                className={inputCls}
              />
            </Row>
            <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
              0 uses the writer default. Smaller row groups let a later read skip more of the file
              via row-group statistics.
            </p>
          </>
        ) : (
          <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
            Arrow IPC output is always uncompressed.
            {opts.format === "arrowFile" &&
              " The Arrow IPC file format is also known as Feather v2."}
          </p>
        )}

        <Row label="Rows to export">
          <select
            value={scopeIdx}
            onChange={(e) => setScopeIdx(Number(e.target.value))}
            className={selectCls}
          >
            {choices.map((c, i) => (
              <option key={c.label} value={i} className="dark:bg-zinc-800">
                {c.label}
              </option>
            ))}
          </select>
        </Row>

        <p className="rounded bg-zinc-50 px-2 py-1.5 text-xs tabular-nums text-zinc-600 dark:bg-zinc-900 dark:text-zinc-300">
          {counts
            ? `Will write ${counts.rows.toLocaleString()} data row${counts.rows === 1 ? "" : "s"} × ${counts.cols} column${counts.cols === 1 ? "" : "s"}`
            : "Counting…"}
        </p>

        <hr className="border-zinc-100 dark:border-zinc-800" />

        <div className="flex flex-wrap gap-x-5 gap-y-2">
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={opts.typed}
              onChange={(e) => patch({ typed: e.target.checked })}
              className="accent-violet-600"
            />
            Typed columns from the declared schema (else all text)
          </label>

          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={opts.backup === "single"}
              onChange={(e) => patch({ backup: e.target.checked ? "single" : "none" })}
              className="accent-violet-600"
            />
            Keep .bak of a replaced file
          </label>
        </div>

        <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
          Typed export preserves nulls (distinct from empty strings), integer widths (i64/u64),
          decimal precision/scale, and timestamp timezones. Cells that don't fit the declared type
          export as NULL and are reported after the write.
        </p>

        {result.error && <p className="text-xs text-red-600 dark:text-red-400">{result.error}</p>}
      </div>
    </Modal>
  );
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-3">
      <span className="text-zinc-500 dark:text-zinc-400">{label}</span>
      {children}
    </div>
  );
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const btnPrimary = "rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500";
const selectCls =
  "rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700";
const inputCls =
  "w-32 rounded border border-zinc-300 bg-transparent px-2 py-1 text-right text-sm tabular-nums outline-none focus:border-violet-500 dark:border-zinc-700";
