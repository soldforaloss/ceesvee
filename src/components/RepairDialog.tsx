import { useEffect, useState } from "react";

import { parseNullTokens, repairApplyLabel, repairIsNoop } from "../lib/repair";
import * as api from "../lib/tauri";
import { useActiveMeta, useStore } from "../store/useStore";
import type { RepairOp, RepairPreview, RepairSpec } from "../types";
import { Modal } from "./Modal";

type OpKey = RepairOp["type"];

const OP_LABELS: Record<OpKey, string> = {
  normalizeNullTokens: "Normalize null tokens to blank",
  fillConstant: "Fill blanks with a constant",
  fillForward: "Fill forward (last value down)",
  fillBackward: "Fill backward (next value up)",
  fillMean: "Fill with column mean",
  fillMedian: "Fill with column median",
  fillMode: "Fill with most frequent value",
  interpolate: "Linear interpolation",
  removeRows: "Remove rows above missing threshold",
  removeColumns: "Remove columns above missing threshold",
};

const DEFAULT_TOKENS = "NA, N/A, null, NULL, -, ?";

/**
 * Missing-value repair (F29): controlled fills, normalizations, and
 * removals from a closed operation set. Everything is previewed (affected
 * counts, computed fill values, before/after examples) before a one-undo
 * apply; row/column removal is explicit and shows exactly what goes.
 */
export function RepairDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const refresh = useStore((s) => s.refreshActiveDoc);
  const selectedCols = useStore((s) => s.selectedCols);
  const selectedRows = useStore((s) => s.selectedRows);

  const [op, setOp] = useState<OpKey>("normalizeNullTokens");
  const [columns, setColumns] = useState<number[]>(() =>
    selectedCols.length > 0 ? selectedCols : [],
  );
  const [scopeKind, setScopeKind] = useState<"all" | "visibleRows" | "selectedRows">("all");
  const [tokens, setTokens] = useState(DEFAULT_TOKENS);
  const [constant, setConstant] = useState("");
  const [groupColumns, setGroupColumns] = useState<number[]>([]);
  const [extrapolate, setExtrapolate] = useState(false);
  const [threshold, setThreshold] = useState(0.5);
  const [preview, setPreview] = useState<RepairPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [working, setWorking] = useState(false);

  // Any input change invalidates the current preview.
  useEffect(() => {
    setPreview(null);
  }, [op, columns, scopeKind, tokens, constant, groupColumns, extrapolate, threshold]);

  if (!meta) return null;
  const readOnly = meta.backing === "indexedReadOnly";
  const headers = meta.headers.map((h, i) => h || `Column ${i + 1}`);
  const removal = op === "removeRows" || op === "removeColumns";
  const stale = preview !== null && preview.revision !== meta.revision;

  const buildOp = (): RepairOp => {
    switch (op) {
      case "normalizeNullTokens":
        return { type: op, tokens: parseNullTokens(tokens) };
      case "fillConstant":
        return { type: op, value: constant };
      case "fillForward":
      case "fillBackward":
        return { type: op, groupColumns };
      case "interpolate":
        return { type: op, extrapolate };
      case "removeRows":
      case "removeColumns":
        return { type: op, threshold };
      default:
        return { type: op };
    }
  };

  const buildSpec = (): RepairSpec => ({
    op: buildOp(),
    columns,
    scope:
      scopeKind === "selectedRows"
        ? { type: "selectedRows", rows: selectedRows }
        : { type: scopeKind },
  });

  const runPreview = async () => {
    setError(null);
    try {
      setPreview(await api.previewRepair(meta.id, buildSpec(), meta.revision));
    } catch (e) {
      setError(String(e));
    }
  };

  const apply = async () => {
    if (!preview) return;
    setWorking(true);
    setError(null);
    try {
      await api.applyRepair(meta.id, buildSpec(), preview.revision);
      await refresh();
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  };

  const toggleColumn = (i: number, set: (next: number[]) => void, list: number[]) =>
    set(list.includes(i) ? list.filter((c) => c !== i) : [...list, i].sort((a, b) => a - b));

  const applyLabel = repairApplyLabel(preview);
  const nothingToDo = repairIsNoop(preview);

  return (
    <Modal
      title="Repair missing values"
      onClose={onClose}
      size="lg"
      footer={
        <>
          <button onClick={onClose} className={btnGhost}>
            Cancel
          </button>
          <button
            onClick={() => void runPreview()}
            disabled={columns.length === 0 || readOnly}
            title={readOnly ? "Read-only (indexed) document" : undefined}
            className={btnGhost}
          >
            Preview
          </button>
          <button
            onClick={() => void apply()}
            disabled={working || readOnly || !preview || stale || nothingToDo}
            title={stale ? "The document changed — preview again" : undefined}
            className={`rounded px-3 py-1.5 text-sm text-white disabled:opacity-40 ${
              removal ? "bg-red-600 hover:bg-red-500" : "bg-violet-600 hover:bg-violet-500"
            }`}
          >
            {working ? "Applying…" : applyLabel}
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <label className="flex items-center gap-2 text-xs">
          Operation
          <select value={op} onChange={(e) => setOp(e.target.value as OpKey)} className={selectCls}>
            {Object.entries(OP_LABELS).map(([value, label]) => (
              <option key={value} value={value} className="dark:bg-zinc-800">
                {label}
              </option>
            ))}
          </select>
        </label>

        {/* Operation parameters */}
        {op === "normalizeNullTokens" && (
          <label className="block text-xs">
            Null tokens (comma-separated, matched exactly after trimming)
            <input
              value={tokens}
              onChange={(e) => setTokens(e.target.value)}
              className={inputCls}
            />
          </label>
        )}
        {op === "fillConstant" && (
          <label className="block text-xs">
            Fill value
            <input
              value={constant}
              onChange={(e) => setConstant(e.target.value)}
              className={inputCls}
            />
          </label>
        )}
        {(op === "fillForward" || op === "fillBackward") && (
          <div className="text-xs">
            <p className="mb-1 text-zinc-500 dark:text-zinc-400">
              Optional grouping columns — a fill never crosses a group boundary:
            </p>
            <div className="flex flex-wrap gap-x-3 gap-y-1">
              {headers.map((h, i) => (
                <label key={i} className="flex items-center gap-1">
                  <input
                    type="checkbox"
                    checked={groupColumns.includes(i)}
                    onChange={() => toggleColumn(i, setGroupColumns, groupColumns)}
                    className="accent-violet-600"
                  />
                  {h}
                </label>
              ))}
            </div>
          </div>
        )}
        {op === "interpolate" && (
          <label className="flex items-center gap-1.5 text-xs">
            <input
              type="checkbox"
              checked={extrapolate}
              onChange={(e) => setExtrapolate(e.target.checked)}
              className="accent-violet-600"
            />
            Extend the first/last known value over leading and trailing blanks
          </label>
        )}
        {removal && (
          <label className="flex items-center gap-2 text-xs">
            Missing-value threshold
            <input
              type="number"
              min={0}
              max={1}
              step={0.05}
              value={threshold}
              onChange={(e) => setThreshold(Number(e.target.value))}
              className="w-20 rounded border border-zinc-300 bg-transparent px-1 py-0.5 dark:border-zinc-600"
            />
            <span className="text-zinc-400">
              ({op === "removeRows" ? "fraction of target columns blank" : "fraction of rows blank"}
              )
            </span>
          </label>
        )}

        {/* Target columns */}
        <div className="text-xs">
          <p className="mb-1 text-zinc-500 dark:text-zinc-400">Target columns:</p>
          <div className="flex max-h-24 flex-wrap gap-x-3 gap-y-1 overflow-y-auto">
            {headers.map((h, i) => (
              <label key={i} className="flex items-center gap-1">
                <input
                  type="checkbox"
                  checked={columns.includes(i)}
                  onChange={() => toggleColumn(i, setColumns, columns)}
                  className="accent-violet-600"
                />
                {h}
              </label>
            ))}
          </div>
          <div className="mt-1 flex gap-2">
            <button
              onClick={() => setColumns(headers.map((_, i) => i))}
              className="text-violet-600 hover:underline dark:text-violet-400"
            >
              all
            </button>
            <button
              onClick={() => setColumns([])}
              className="text-violet-600 hover:underline dark:text-violet-400"
            >
              none
            </button>
          </div>
        </div>

        {/* Row scope */}
        <div className="flex items-center gap-3 text-xs">
          Rows:
          {(["all", "visibleRows", "selectedRows"] as const).map((kind) => (
            <label key={kind} className="flex items-center gap-1">
              <input
                type="radio"
                name="repair-scope"
                checked={scopeKind === kind}
                onChange={() => setScopeKind(kind)}
                disabled={
                  (kind === "visibleRows" && !meta.filtered) ||
                  (kind === "selectedRows" && selectedRows.length === 0)
                }
                className="accent-violet-600"
              />
              {kind === "all" ? "all" : kind === "visibleRows" ? "visible only" : "selected"}
            </label>
          ))}
        </div>

        {error && <p className="text-xs text-red-600 dark:text-red-400">{error}</p>}
        {stale && (
          <p className="rounded bg-amber-50 px-2 py-1.5 text-xs text-amber-700 dark:bg-amber-500/10 dark:text-amber-300">
            The document changed since this preview — preview again before applying.
          </p>
        )}

        {/* Preview */}
        {preview && (
          <div className="rounded bg-zinc-50 p-2 text-xs dark:bg-zinc-900">
            <p className="font-medium">
              {preview.cellsAffected > 0 &&
                `${preview.cellsAffected.toLocaleString()} cell${preview.cellsAffected === 1 ? "" : "s"} would change.`}
              {preview.rowsRemoved > 0 &&
                ` ${preview.rowsRemoved.toLocaleString()} row${preview.rowsRemoved === 1 ? "" : "s"} would be removed.`}
              {preview.columnsRemoved > 0 &&
                ` ${preview.columnsRemoved.toLocaleString()} column${preview.columnsRemoved === 1 ? "" : "s"} would be removed.`}
              {nothingToDo && "Nothing to change with these settings."}
            </p>
            {preview.fillValues.length > 0 && (
              <p className="mt-1 text-zinc-500 dark:text-zinc-400">
                Fill values:{" "}
                {preview.fillValues
                  .map(([c, v]) => `${headers[c] ?? `Column ${c + 1}`} → ${v}`)
                  .join(" · ")}
              </p>
            )}
            {preview.invalidNumeric > 0 && (
              <p className="mt-1 text-amber-600 dark:text-amber-400">
                {preview.invalidNumeric.toLocaleString()} non-numeric value
                {preview.invalidNumeric === 1 ? "" : "s"} ignored by the statistics.
              </p>
            )}
            {preview.examples.length > 0 && (
              <ul className="mt-1 space-y-0.5 font-mono text-[11px] text-zinc-500 dark:text-zinc-400">
                {preview.examples.slice(0, 8).map((ex, i) => (
                  <li key={i} className="truncate">
                    row {ex.row + 1}, {headers[ex.col] ?? ex.col}:{" "}
                    {ex.before === "" ? "∅" : ex.before} → {ex.after === "" ? "∅" : ex.after}
                  </li>
                ))}
              </ul>
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
  "mt-1 w-full rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 outline-none focus:border-violet-500 dark:border-zinc-600";
