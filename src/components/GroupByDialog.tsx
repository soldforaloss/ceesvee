import { useState } from "react";

import { aggregateNeedsColumn, normalizeAggregates, usesConcat } from "../lib/groupby";
import * as api from "../lib/tauri";
import { useActiveMeta, useStore } from "../store/useStore";
import type { Aggregate, AggregateSpec, GroupByPreview, GroupBySpec } from "../types";
import { Modal } from "./Modal";

const AGG_LABELS: Record<Aggregate, string> = {
  count: "Row count",
  countNonBlank: "Non-blank count",
  countDistinct: "Distinct count",
  sum: "Sum",
  mean: "Mean",
  min: "Minimum",
  max: "Maximum",
  median: "Median",
  first: "First",
  last: "Last",
  concat: "Concatenate",
  concatDistinct: "Concatenate distinct",
};

/**
 * Group-by aggregations (F22): summarise the active document into a NEW
 * grouped document (the source is untouched). A closed aggregate set with
 * explicit blank-key and invalid-numeric policies, previewed before running.
 */
export function GroupByDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const derive = useStore((s) => s.derive);
  const deriveError = useStore((s) => s.deriveError);
  const trackDerive = useStore((s) => s.trackDerive);
  const cancelDerive = useStore((s) => s.cancelDerive);

  const [groupColumns, setGroupColumns] = useState<number[]>([0]);
  const [aggregates, setAggregates] = useState<AggregateSpec[]>([
    { aggregate: "count", column: null, outputName: null },
  ]);
  const [scopeVisible, setScopeVisible] = useState(false);
  const [normalized, setNormalized] = useState(false);
  const [blankKeys, setBlankKeys] = useState<"keep" | "exclude">("keep");
  const [ordering, setOrdering] = useState<"byKey" | "byCountDesc" | "firstSeen">("byKey");
  const [separator, setSeparator] = useState(", ");
  const [preview, setPreview] = useState<GroupByPreview | null>(null);
  const [error, setError] = useState<string | null>(null);

  const running = derive?.kind === "groupBy";

  if (!meta) return null;
  const headers = meta.headers.map((h, i) => h || `Column ${i + 1}`);
  const showSeparator = usesConcat(aggregates);

  const buildSpec = (): GroupBySpec => ({
    groupColumns,
    aggregates: normalizeAggregates(aggregates),
    scope: scopeVisible ? { type: "visibleRows" } : { type: "all" },
    normalizedGrouping: normalized,
    blankKeys,
    ordering,
    concatSeparator: separator,
    concatMaxLen: 2000,
  });

  const invalidate = () => setPreview(null);

  const runPreview = async () => {
    setError(null);
    try {
      setPreview(await api.previewGroupBy(meta.id, buildSpec(), meta.revision));
    } catch (e) {
      setError(String(e));
      setPreview(null);
    }
  };

  const run = async () => {
    setError(null);
    try {
      const started = await api.startGroupBy(meta.id, buildSpec(), meta.revision);
      trackDerive(started.jobId, started.docId, "groupBy");
    } catch (e) {
      setError(String(e));
    }
  };

  const patchAgg = (i: number, patch: Partial<AggregateSpec>) => {
    setAggregates((a) => a.map((agg, j) => (j === i ? { ...agg, ...patch } : agg)));
    invalidate();
  };

  return (
    <Modal
      title="Group by"
      onClose={onClose}
      size="xl"
      footer={
        <>
          <button onClick={onClose} className={btnGhost}>
            Close
          </button>
          <button
            onClick={() => void runPreview()}
            disabled={groupColumns.length === 0 || aggregates.length === 0 || running}
            className={btnGhost}
          >
            Preview
          </button>
          <button
            onClick={() => void run()}
            disabled={running || !preview}
            title={!preview ? "Preview first" : undefined}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            {running ? "Grouping…" : "Group into a new document"}
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <div className="text-xs">
          <p className="mb-1 text-zinc-500 dark:text-zinc-400">Group by:</p>
          <div className="flex max-h-16 flex-wrap gap-x-3 gap-y-1 overflow-y-auto">
            {headers.map((h, i) => (
              <label key={i} className="flex items-center gap-1">
                <input
                  type="checkbox"
                  checked={groupColumns.includes(i)}
                  onChange={(e) => {
                    setGroupColumns(
                      e.target.checked
                        ? [...groupColumns, i].sort((a, b) => a - b)
                        : groupColumns.filter((c) => c !== i),
                    );
                    invalidate();
                  }}
                  className="accent-violet-600"
                />
                {h}
              </label>
            ))}
          </div>
        </div>

        <div className="text-xs">
          <p className="mb-1 text-zinc-500 dark:text-zinc-400">Aggregates:</p>
          <div className="space-y-1.5">
            {aggregates.map((agg, i) => (
              <div key={i} className="flex flex-wrap items-center gap-2">
                <select
                  value={agg.aggregate}
                  onChange={(e) => patchAgg(i, { aggregate: e.target.value as Aggregate })}
                  className={selectCls}
                >
                  {Object.entries(AGG_LABELS).map(([value, label]) => (
                    <option key={value} value={value} className="dark:bg-zinc-800">
                      {label}
                    </option>
                  ))}
                </select>
                {aggregateNeedsColumn(agg.aggregate) && (
                  <select
                    value={agg.column ?? 0}
                    onChange={(e) => patchAgg(i, { column: Number(e.target.value) })}
                    className={selectCls}
                  >
                    {headers.map((h, ci) => (
                      <option key={ci} value={ci} className="dark:bg-zinc-800">
                        {h}
                      </option>
                    ))}
                  </select>
                )}
                <input
                  value={agg.outputName ?? ""}
                  onChange={(e) => patchAgg(i, { outputName: e.target.value || null })}
                  placeholder="output name (optional)"
                  className="w-44 rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 dark:border-zinc-600"
                />
                {aggregates.length > 1 && (
                  <button
                    onClick={() => {
                      setAggregates(aggregates.filter((_, j) => j !== i));
                      invalidate();
                    }}
                    className="text-red-600 hover:underline dark:text-red-400"
                  >
                    remove
                  </button>
                )}
              </div>
            ))}
          </div>
          <button
            onClick={() => {
              setAggregates([...aggregates, { aggregate: "sum", column: 0, outputName: null }]);
              invalidate();
            }}
            className={`${chipBtn} mt-1.5`}
          >
            + aggregate
          </button>
        </div>

        <div className="flex flex-wrap items-center gap-3 text-xs">
          <label className="flex items-center gap-1.5">
            Blank keys
            <select
              value={blankKeys}
              onChange={(e) => {
                setBlankKeys(e.target.value as "keep" | "exclude");
                invalidate();
              }}
              className={selectCls}
            >
              <option value="keep" className="dark:bg-zinc-800">
                keep as a group
              </option>
              <option value="exclude" className="dark:bg-zinc-800">
                exclude those rows
              </option>
            </select>
          </label>
          <label className="flex items-center gap-1.5">
            Order groups
            <select
              value={ordering}
              onChange={(e) => {
                setOrdering(e.target.value as typeof ordering);
                invalidate();
              }}
              className={selectCls}
            >
              <option value="byKey" className="dark:bg-zinc-800">
                by key
              </option>
              <option value="byCountDesc" className="dark:bg-zinc-800">
                largest first
              </option>
              <option value="firstSeen" className="dark:bg-zinc-800">
                first seen
              </option>
            </select>
          </label>
          <label className="flex items-center gap-1.5">
            <input
              type="checkbox"
              checked={normalized}
              onChange={(e) => {
                setNormalized(e.target.checked);
                invalidate();
              }}
              className="accent-violet-600"
            />
            Case-insensitive grouping
          </label>
          {meta.filtered && (
            <label className="flex items-center gap-1.5">
              <input
                type="checkbox"
                checked={scopeVisible}
                onChange={(e) => {
                  setScopeVisible(e.target.checked);
                  invalidate();
                }}
                className="accent-violet-600"
              />
              Visible rows only
            </label>
          )}
          {showSeparator && (
            <label className="flex items-center gap-1.5">
              Separator
              <input
                value={separator}
                onChange={(e) => {
                  setSeparator(e.target.value);
                  invalidate();
                }}
                className="w-16 rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 dark:border-zinc-600"
              />
            </label>
          )}
        </div>

        {(error ?? deriveError) && (
          <p className="text-xs text-red-600 dark:text-red-400">{error ?? deriveError}</p>
        )}

        {running && derive && (
          <div className="flex items-center gap-3 text-xs text-zinc-500 dark:text-zinc-400">
            <span>
              grouping — {derive.processed.toLocaleString()}
              {derive.total != null && ` / ${derive.total.toLocaleString()}`} rows
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
          <div className="space-y-1.5 rounded bg-zinc-50 p-2 text-xs dark:bg-zinc-900">
            <p className="font-medium">
              {preview.groupCount.toLocaleString()} group
              {preview.groupCount === 1 ? "" : "s"} from {preview.scannedRows.toLocaleString()} rows
              {preview.invalidNumeric > 0 &&
                ` · ${preview.invalidNumeric.toLocaleString()} non-numeric ignored`}
              {preview.blankKeyRows > 0 &&
                ` · ${preview.blankKeyRows.toLocaleString()} blank-key rows excluded`}
            </p>
            <div className="overflow-x-auto">
              <table className="w-full text-left font-mono text-[11px]">
                <thead className="text-zinc-400">
                  <tr>
                    {preview.outputColumns.map((h, i) => (
                      <th key={i} className="pr-3 font-normal">
                        {h}
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {preview.sample.map((row, i) => (
                    <tr key={i} className="text-zinc-600 dark:text-zinc-300">
                      {row.map((cell, j) => (
                        <td key={j} className="max-w-[12rem] truncate pr-3">
                          {cell}
                        </td>
                      ))}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
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
const chipBtn =
  "rounded border border-zinc-200 px-1.5 py-0.5 text-[11px] hover:border-violet-400 disabled:opacity-40 dark:border-zinc-700";
