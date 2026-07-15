import { useMemo, useState } from "react";

import { scopeChoices } from "../lib/export";
import * as api from "../lib/tauri";
import {
  buildTransformSpec,
  defaultValues,
  TRANSFORMS,
  type ParamValues,
  type TransformKind,
} from "../lib/transforms";
import { useActiveMeta, useStore } from "../store/useStore";
import type { TransformErrorPolicy, TransformPreview } from "../types";
import { ColumnsPicker } from "./ColumnsPicker";
import { Modal } from "./Modal";

/**
 * Previewable data cleaning (F06): pick an operation and scope, preview the
 * exact effect (counts, before/after examples, parse failures, column
 * changes), then apply as one undoable step. No formulas, no scripting.
 */
export function TransformDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const applyTransformSpec = useStore((s) => s.applyTransformSpec);
  const filtered = useStore((s) => s.tabs.find((t) => t.id === s.activeId)?.filtered ?? false);
  const selectionRect = useStore((s) => s.selectionRect);
  const selectedRows = useStore((s) => s.selectedRows);
  const selectedCols = useStore((s) => s.selectedCols);

  const [kind, setKind] = useState<TransformKind>("trim");
  const def = TRANSFORMS.find((t) => t.type === kind)!;
  const [values, setValues] = useState<ParamValues>(() => defaultValues(def));
  const [policy, setPolicy] = useState<TransformErrorPolicy>("failAll");

  const choices = useMemo(
    () => scopeChoices(filtered, selectionRect, selectedRows, selectedCols),
    [filtered, selectionRect, selectedRows, selectedCols],
  );
  const [scopeIdx, setScopeIdx] = useState(0);
  const scope = (choices[scopeIdx] ?? choices[0]).scope;

  const [preview, setPreview] = useState<TransformPreview | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [working, setWorking] = useState(false);

  if (!meta) return null;

  const pickKind = (next: TransformKind) => {
    setKind(next);
    const nextDef = TRANSFORMS.find((t) => t.type === next)!;
    setValues(defaultValues(nextDef));
    setPreview(null);
    setPreviewError(null);
  };

  const setValue = (key: string, value: ParamValues[string]) => {
    setValues((v) => ({ ...v, [key]: value }));
    setPreview(null);
  };

  const spec = buildTransformSpec(kind, values);
  const specError = "error" in spec ? spec.error : null;

  const runPreview = async () => {
    if ("error" in spec) return;
    setWorking(true);
    setPreviewError(null);
    try {
      setPreview(await api.previewTransform(meta.id, spec, scope, meta.revision));
    } catch (e) {
      setPreview(null);
      setPreviewError(String(e));
    } finally {
      setWorking(false);
    }
  };

  const runApply = async () => {
    if ("error" in spec || !preview) return;
    setWorking(true);
    const ok = await applyTransformSpec(spec, scope, policy, preview.expectedRevision);
    setWorking(false);
    if (ok) onClose();
    else setPreview(null); // likely stale: force a fresh preview
  };

  const canApply =
    preview !== null &&
    !working &&
    (preview.parseFailures === 0 || policy === "skipInvalid") &&
    (preview.affectedCells > 0 || preview.columnsInserted.length > 0);

  return (
    <Modal
      title="Clean data"
      onClose={onClose}
      size="xl"
      footer={
        <>
          <button onClick={onClose} className={btnGhost}>
            Cancel
          </button>
          <button
            onClick={() => void runPreview()}
            disabled={working || specError !== null}
            className={btnGhost}
          >
            {working && !preview ? "Previewing…" : "Preview"}
          </button>
          <button
            onClick={() => void runApply()}
            disabled={!canApply}
            title={preview ? undefined : "Preview first"}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            {working && preview ? "Applying…" : "Apply"}
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <div className="flex flex-wrap items-center gap-3">
          <select
            value={kind}
            onChange={(e) => pickKind(e.target.value as TransformKind)}
            className={selectCls}
          >
            {TRANSFORMS.map((t) => (
              <option key={t.type} value={t.type} className="dark:bg-zinc-800">
                {t.label}
              </option>
            ))}
          </select>

          {!def.structural && (
            <select
              value={scopeIdx}
              onChange={(e) => {
                setScopeIdx(Number(e.target.value));
                setPreview(null);
              }}
              className={selectCls}
            >
              {choices.map((c, i) => (
                <option key={c.label} value={i} className="dark:bg-zinc-800">
                  {c.label}
                </option>
              ))}
            </select>
          )}
        </div>

        {def.params.length > 0 && (
          <div className="flex flex-wrap items-center gap-x-5 gap-y-2">
            {def.params.map((p) => (
              <label key={p.key} className="flex items-center gap-1.5 text-xs">
                {p.kind !== "checkbox" && (
                  <span className="text-zinc-500 dark:text-zinc-400">{p.label}</span>
                )}
                {p.kind === "text" && (
                  <input
                    value={String(values[p.key] ?? "")}
                    onChange={(e) => setValue(p.key, e.target.value)}
                    placeholder={p.placeholder}
                    className={inputCls}
                  />
                )}
                {p.kind === "checkbox" && (
                  <>
                    <input
                      type="checkbox"
                      checked={values[p.key] === true}
                      onChange={(e) => setValue(p.key, e.target.checked)}
                      className="accent-violet-600"
                    />
                    <span>{p.label}</span>
                  </>
                )}
                {p.kind === "column" && (
                  <select
                    value={Number(values[p.key] ?? 0)}
                    onChange={(e) => setValue(p.key, Number(e.target.value))}
                    className={selectCls}
                  >
                    {meta.headers.map((h, i) => (
                      <option key={i} value={i} className="dark:bg-zinc-800">
                        {h.trim() || `Column ${i + 1}`}
                      </option>
                    ))}
                  </select>
                )}
                {p.kind === "columns" && (
                  <ColumnsPicker
                    headers={meta.headers}
                    value={Array.isArray(values[p.key]) ? (values[p.key] as number[]) : []}
                    onChange={(cols) => setValue(p.key, cols)}
                  />
                )}
              </label>
            ))}
          </div>
        )}

        <div className="flex items-center gap-4 text-xs">
          <span className="text-zinc-500 dark:text-zinc-400">On conversion failures</span>
          {(
            [
              ["failAll", "Fail (change nothing)"],
              ["skipInvalid", "Skip invalid cells"],
            ] as const
          ).map(([value, label]) => (
            <label key={value} className="flex cursor-pointer items-center gap-1.5">
              <input
                type="radio"
                checked={policy === value}
                onChange={() => setPolicy(value)}
                className="accent-violet-600"
              />
              {label}
            </label>
          ))}
        </div>

        {(specError || previewError) && (
          <p className="rounded border border-red-200 bg-red-50 px-2 py-1.5 text-xs text-red-700 dark:border-red-900/60 dark:bg-red-950/50 dark:text-red-300">
            {specError ?? previewError}
          </p>
        )}

        {preview && (
          <div className="space-y-2 rounded border border-zinc-200 px-3 py-2 text-xs dark:border-zinc-800">
            <div className="flex flex-wrap gap-x-4 gap-y-1 tabular-nums">
              <span>
                <span className="font-semibold">{preview.affectedCells.toLocaleString()}</span>{" "}
                {def.structural ? "rows affected" : "cells will change"}
              </span>
              {preview.parseFailures > 0 && (
                <span className="text-amber-600 dark:text-amber-400">
                  {preview.parseFailures.toLocaleString()} cannot be converted
                </span>
              )}
            </div>

            {(preview.columnsRemoved.length > 0 || preview.columnsInserted.length > 0) && (
              <p className="text-zinc-500 dark:text-zinc-400">
                {preview.columnsRemoved.length > 0 &&
                  `Removes: ${preview.columnsRemoved.join(", ")}. `}
                {preview.columnsInserted.length > 0 &&
                  `Inserts: ${preview.columnsInserted.join(", ")}.`}
                {preview.appliesToAllRows && " Affects every row (column structure)."}
              </p>
            )}

            {preview.examples.length > 0 && (
              <table className="w-full border-collapse">
                <thead>
                  <tr className="text-left text-[10px] uppercase tracking-wide text-zinc-400">
                    <th className="py-0.5 pr-2 font-medium">Row</th>
                    <th className="py-0.5 pr-2 font-medium">Before</th>
                    <th className="py-0.5 font-medium">After</th>
                  </tr>
                </thead>
                <tbody>
                  {preview.examples.map((ex, i) => (
                    <tr key={i} className="border-t border-zinc-100 dark:border-zinc-800/60">
                      <td className="py-0.5 pr-2 tabular-nums text-zinc-400">{ex.row + 1}</td>
                      <td
                        className="max-w-[14rem] truncate py-0.5 pr-2 font-mono"
                        title={ex.before}
                      >
                        {ex.before || <em className="text-zinc-400">(empty)</em>}
                      </td>
                      <td className="max-w-[14rem] truncate py-0.5 font-mono" title={ex.after}>
                        {ex.after}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}

            {preview.failureExamples.length > 0 && (
              <div>
                <div className="mb-0.5 text-amber-600 dark:text-amber-400">
                  Cannot be converted:
                </div>
                <ul className="space-y-0.5">
                  {preview.failureExamples.map((ex, i) => (
                    <li key={i} className="truncate font-mono text-zinc-500">
                      row {ex.row + 1}: “{ex.before}”
                    </li>
                  ))}
                </ul>
              </div>
            )}
          </div>
        )}
      </div>
    </Modal>
  );
}

const selectCls =
  "rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700";
const inputCls =
  "w-40 rounded border border-zinc-300 bg-transparent px-1.5 py-1 font-mono text-xs outline-none focus:border-violet-500 dark:border-zinc-700";
const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 disabled:opacity-40 dark:text-zinc-300 dark:hover:bg-zinc-800";
