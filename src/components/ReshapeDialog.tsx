import { useState } from "react";

import { reshapeProblem } from "../lib/reshape";
import * as api from "../lib/tauri";
import { useActiveMeta, useStore } from "../store/useStore";
import type { PivotAgg, ReshapePreview, ReshapeSpec } from "../types";
import { Modal } from "./Modal";

type Mode = "unpivot" | "pivot" | "transpose";

const PIVOT_AGGS: Record<PivotAgg, string> = {
  none: "none (unique values only)",
  count: "count",
  countNonBlank: "count non-blank",
  sum: "sum",
  mean: "mean",
  median: "median",
  min: "min",
  max: "max",
  first: "first",
  last: "last",
};

const COLUMN_LIMIT = 1000;

/**
 * Reshape (F23): unpivot wide → long, pivot long → wide, or transpose —
 * each into a NEW document with the source untouched. Deterministic pivot
 * column order, duplicate-coordinate detection, and size guards with an
 * explicit confirm to raise the column limit.
 */
export function ReshapeDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const derive = useStore((s) => s.derive);
  const deriveError = useStore((s) => s.deriveError);
  const trackDerive = useStore((s) => s.trackDerive);
  const cancelDerive = useStore((s) => s.cancelDerive);

  const [mode, setMode] = useState<Mode>("unpivot");
  const [idColumns, setIdColumns] = useState<number[]>([0]);
  const [valueColumns, setValueColumns] = useState<number[]>([]);
  const [attributeName, setAttributeName] = useState("attribute");
  const [valueName, setValueName] = useState("value");
  const [omitBlanks, setOmitBlanks] = useState(false);
  const [addSourceRow, setAddSourceRow] = useState(false);
  const [rowKeys, setRowKeys] = useState<number[]>([0]);
  const [headerColumn, setHeaderColumn] = useState(1);
  const [valueColumn, setValueColumn] = useState(2);
  const [aggregation, setAggregation] = useState<PivotAgg>("none");
  const [confirmedColumns, setConfirmedColumns] = useState(false);
  const [preview, setPreview] = useState<ReshapePreview | null>(null);
  const [error, setError] = useState<string | null>(null);

  const running = derive?.kind === "reshape";

  if (!meta) return null;
  const headers = meta.headers.map((h, i) => h || `Column ${i + 1}`);

  const buildSpec = (raiseLimit: boolean): ReshapeSpec => {
    const maxColumns = raiseLimit ? Number.MAX_SAFE_INTEGER : COLUMN_LIMIT;
    switch (mode) {
      case "unpivot":
        return {
          type: "unpivot",
          idColumns,
          valueColumns,
          attributeName,
          valueName,
          omitBlanks,
          addSourceRow,
        };
      case "pivot":
        return {
          type: "pivot",
          rowKeys,
          headerColumn,
          valueColumn,
          aggregation,
          maxColumns,
        };
      case "transpose":
        return { type: "transpose", maxColumns };
    }
  };

  const invalidate = () => {
    setPreview(null);
    setConfirmedColumns(false);
  };

  const runPreview = async () => {
    setError(null);
    try {
      setPreview(await api.previewReshape(meta.id, buildSpec(false), meta.revision));
    } catch (e) {
      setError(String(e));
      setPreview(null);
    }
  };

  const run = async () => {
    setError(null);
    try {
      const started = await api.startReshape(meta.id, buildSpec(confirmedColumns), meta.revision);
      trackDerive(started.jobId, started.docId, "reshape");
    } catch (e) {
      setError(String(e));
    }
  };

  const overLimit = preview?.overColumnLimit ?? false;
  const noneConflict =
    mode === "pivot" && aggregation === "none" && (preview?.duplicateCoordinates ?? 0) > 0;

  const multiCols = (selected: number[], set: (next: number[]) => void, disabled?: number[]) => (
    <div className="flex max-h-16 flex-wrap gap-x-3 gap-y-1 overflow-y-auto">
      {headers.map((h, i) => (
        <label key={i} className="flex items-center gap-1">
          <input
            type="checkbox"
            checked={selected.includes(i)}
            disabled={disabled?.includes(i)}
            onChange={(e) => {
              set(
                e.target.checked
                  ? [...selected, i].sort((a, b) => a - b)
                  : selected.filter((c) => c !== i),
              );
              invalidate();
            }}
            className="accent-violet-600"
          />
          {h}
        </label>
      ))}
    </div>
  );

  const colSelect = (value: number, set: (next: number) => void) => (
    <select
      value={value}
      onChange={(e) => {
        set(Number(e.target.value));
        invalidate();
      }}
      className={selectCls}
    >
      {headers.map((h, i) => (
        <option key={i} value={i} className="dark:bg-zinc-800">
          {h}
        </option>
      ))}
    </select>
  );

  return (
    <Modal
      title="Reshape"
      onClose={onClose}
      size="xl"
      footer={
        <>
          <button onClick={onClose} className={btnGhost}>
            Close
          </button>
          <button
            onClick={() => void runPreview()}
            disabled={running || reshapeProblem(buildSpec(false)) !== null}
            title={reshapeProblem(buildSpec(false)) ?? undefined}
            className={btnGhost}
          >
            Preview
          </button>
          <button
            onClick={() => {
              if (overLimit && !confirmedColumns) {
                setConfirmedColumns(true);
                return;
              }
              void run();
            }}
            disabled={running || !preview || noneConflict}
            title={
              noneConflict
                ? "Duplicate pivot coordinates — pick an aggregation"
                : !preview
                  ? "Preview first"
                  : undefined
            }
            className={`rounded px-3 py-1.5 text-sm text-white disabled:opacity-40 ${
              overLimit && !confirmedColumns
                ? "bg-amber-600 hover:bg-amber-500"
                : "bg-violet-600 hover:bg-violet-500"
            }`}
          >
            {running
              ? "Reshaping…"
              : overLimit && !confirmedColumns
                ? `${preview?.outputColumns.toLocaleString()} columns — confirm`
                : "Reshape into a new document"}
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <div className="flex overflow-hidden rounded border border-zinc-200 text-xs dark:border-zinc-700">
          {(["unpivot", "pivot", "transpose"] as const).map((m) => (
            <button
              key={m}
              onClick={() => {
                setMode(m);
                invalidate();
              }}
              className={`px-3 py-1 capitalize ${
                mode === m
                  ? "bg-violet-600 text-white"
                  : "text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"
              }`}
            >
              {m}
            </button>
          ))}
        </div>

        {mode === "unpivot" && (
          <div className="space-y-2 text-xs">
            <div>
              <p className="mb-1 text-zinc-500 dark:text-zinc-400">Keep as identifiers:</p>
              {multiCols(idColumns, setIdColumns, valueColumns)}
            </div>
            <div>
              <p className="mb-1 text-zinc-500 dark:text-zinc-400">Unpivot into rows:</p>
              {multiCols(valueColumns, setValueColumns, idColumns)}
            </div>
            <div className="flex flex-wrap items-center gap-3">
              <label className="flex items-center gap-1.5">
                Attribute column
                <input
                  value={attributeName}
                  onChange={(e) => {
                    setAttributeName(e.target.value);
                    invalidate();
                  }}
                  className={inputCls}
                />
              </label>
              <label className="flex items-center gap-1.5">
                Value column
                <input
                  value={valueName}
                  onChange={(e) => {
                    setValueName(e.target.value);
                    invalidate();
                  }}
                  className={inputCls}
                />
              </label>
              <label className="flex items-center gap-1.5">
                <input
                  type="checkbox"
                  checked={omitBlanks}
                  onChange={(e) => {
                    setOmitBlanks(e.target.checked);
                    invalidate();
                  }}
                  className="accent-violet-600"
                />
                Omit blank values
              </label>
              <label className="flex items-center gap-1.5">
                <input
                  type="checkbox"
                  checked={addSourceRow}
                  onChange={(e) => {
                    setAddSourceRow(e.target.checked);
                    invalidate();
                  }}
                  className="accent-violet-600"
                />
                Add source-row column
              </label>
            </div>
          </div>
        )}

        {mode === "pivot" && (
          <div className="space-y-2 text-xs">
            <div>
              <p className="mb-1 text-zinc-500 dark:text-zinc-400">Row keys:</p>
              {multiCols(rowKeys, setRowKeys, [headerColumn, valueColumn])}
            </div>
            <div className="flex flex-wrap items-center gap-3">
              <label className="flex items-center gap-1.5">
                Headers from {colSelect(headerColumn, setHeaderColumn)}
              </label>
              <label className="flex items-center gap-1.5">
                Values from {colSelect(valueColumn, setValueColumn)}
              </label>
              <label className="flex items-center gap-1.5">
                Aggregation
                <select
                  value={aggregation}
                  onChange={(e) => {
                    setAggregation(e.target.value as PivotAgg);
                    invalidate();
                  }}
                  className={selectCls}
                >
                  {Object.entries(PIVOT_AGGS).map(([value, label]) => (
                    <option key={value} value={value} className="dark:bg-zinc-800">
                      {label}
                    </option>
                  ))}
                </select>
              </label>
            </div>
          </div>
        )}

        {mode === "transpose" && (
          <p className="text-xs text-zinc-500 dark:text-zinc-400">
            Swap rows and columns: headers become the first column, each row becomes a column.
          </p>
        )}

        {(error ?? deriveError) && (
          <p className="text-xs text-red-600 dark:text-red-400">{error ?? deriveError}</p>
        )}

        {running && derive && (
          <div className="flex items-center gap-3 text-xs text-zinc-500 dark:text-zinc-400">
            <span>
              reshaping — {derive.processed.toLocaleString()}
              {derive.total != null && ` / ${derive.total.toLocaleString()}`}
            </span>
            <button
              onClick={() => void cancelDerive()}
              className="rounded px-2 py-1 text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10"
            >
              Cancel
            </button>
          </div>
        )}

        {preview && (
          <div className="space-y-1 rounded bg-zinc-50 p-2 text-xs dark:bg-zinc-900">
            <p className="font-medium">
              {preview.projectedRows.toLocaleString()} rows ×{" "}
              {preview.outputColumns.toLocaleString()} columns
              {preview.blanksOmitted > 0 &&
                ` · ${preview.blanksOmitted.toLocaleString()} blanks omitted`}
            </p>
            {noneConflict && (
              <p className="text-red-600 dark:text-red-400">
                {preview.duplicateCoordinates.toLocaleString()} cell
                {preview.duplicateCoordinates === 1 ? "" : "s"} would hold more than one value —
                pick an aggregation other than "none".
              </p>
            )}
            {preview.columnSample.length > 0 && (
              <p className="truncate font-mono text-[11px] text-zinc-500 dark:text-zinc-400">
                {preview.columnSample.join(" · ")}
                {preview.outputColumns > preview.columnSample.length && " · …"}
              </p>
            )}
          </div>
        )}
      </div>
    </Modal>
  );
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const selectCls =
  "rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 outline-none focus:border-violet-500 dark:border-zinc-700";
const inputCls =
  "w-32 rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 outline-none focus:border-violet-500 dark:border-zinc-600";
