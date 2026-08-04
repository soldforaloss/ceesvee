import { useMemo, useState } from "react";

import {
  chunkUnitLabel,
  columnDepth,
  columnarFormatLabel,
  columnarOpenPlan,
  defaultColumnarOpenOptions,
  effectivePolicy,
  estimatedMemoryLabel,
  leafName,
  setFieldPolicy,
} from "../lib/columnar";
import { formatBytes } from "../lib/save";
import { LOGICAL_TYPE_LABELS } from "../lib/schema";
import { useStore } from "../store/useStore";
import type { ColumnarOpenOptions, ComplexPolicy } from "../types";
import { Modal } from "./Modal";

/**
 * Upper bound on how many schema rows the dialog renders at once. The open
 * itself is unaffected (the backend maps every column); this only keeps the
 * DOM bounded, per the "bounded windows to React only" invariant — a wide
 * columnar file can be thousands of flattened columns.
 */
const MAX_SCHEMA_ROWS = 300;

/**
 * Parquet / Arrow inspect + open dialog (F32). Self-driven by the
 * `columnarOpen` store slice, so opening a `.parquet` / `.arrow` / `.feather`
 * / `.ipc` file shows it automatically. Reports the container, row and
 * row-group/batch counts, compression, the F31-mapped schema (nested fields
 * indented), the editable-memory estimate, and per-field policy pickers for
 * complex (list/map/struct-as-JSON) fields. The two open modes — indexed
 * read-only vs converted-editable — mirror the read side's constraints
 * (exploding a list forces an editable open, at most one per open).
 */
export function ParquetInspectDialog() {
  const st = useStore((s) => s.columnarOpen);
  const dismiss = useStore((s) => s.dismissColumnarInspect);
  const openIndexed = useStore((s) => s.columnarOpenIndexed);
  const openEditable = useStore((s) => s.columnarOpenEditable);

  const [options, setOptions] = useState<ColumnarOpenOptions>(() => defaultColumnarOpenOptions());
  // Two-step acknowledgement for a large editable open (mirrors OpenModeDialog).
  const [ackEditable, setAckEditable] = useState(false);

  const inspection = st?.inspection ?? null;
  const plan = useMemo(
    () => (inspection ? columnarOpenPlan(inspection, options) : null),
    [inspection, options],
  );

  if (!st) return null;

  const shownColumns = inspection ? inspection.columns.slice(0, MAX_SCHEMA_ROWS) : [];
  const hiddenColumns = (inspection?.columns.length ?? 0) - shownColumns.length;
  const needsDecision = inspection?.needsDecision ?? false;
  const blocked = (plan?.errors.length ?? 0) > 0;
  const indexedDisabled = blocked || plan?.requiresEditable === true || !inspection;
  const isArrow = inspection?.format !== "parquet";

  const setPolicy = (path: string, policy: ComplexPolicy) => {
    setOptions((o) => setFieldPolicy(o, path, policy));
    setAckEditable(false);
  };
  const setDefaultPolicy = (policy: ComplexPolicy) => {
    setOptions((o) => ({ ...o, complexPolicy: policy }));
    setAckEditable(false);
  };

  const doEditable = () => {
    if (blocked) return;
    if (needsDecision && !ackEditable) {
      setAckEditable(true);
      return;
    }
    void openEditable(options, needsDecision);
  };

  return (
    <Modal
      title={`Open Parquet / Arrow — ${st.fileName}`}
      onClose={dismiss}
      size="xl"
      footer={
        <>
          <button onClick={dismiss} className={btnGhost}>
            Cancel
          </button>
          <button
            onClick={() => void openIndexed(options)}
            disabled={indexedDisabled}
            title={plan?.indexedDisabledReason ?? plan?.errors[0] ?? undefined}
            className={btnGhost + " disabled:opacity-40"}
          >
            Open read-only (indexed)
          </button>
          <button
            onClick={doEditable}
            disabled={blocked || !inspection}
            className={
              needsDecision
                ? "rounded bg-amber-600 px-3 py-1.5 text-sm text-white hover:bg-amber-500 disabled:opacity-40"
                : "rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
            }
          >
            {needsDecision && ackEditable
              ? "Open editable anyway"
              : plan?.requiresEditable
                ? "Open editable (explode)"
                : "Convert to editable"}
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        {st.loading && !inspection && (
          <p className="text-zinc-500 dark:text-zinc-400">Inspecting file…</p>
        )}
        {st.error && <p className="text-xs text-red-600 dark:text-red-400">{st.error}</p>}

        {inspection && (
          <>
            {/* Summary */}
            <div className="flex flex-wrap items-center gap-x-4 gap-y-1 rounded bg-zinc-50 px-3 py-2 text-xs dark:bg-zinc-900">
              <span>
                Format:{" "}
                <span className="font-medium">{columnarFormatLabel(inspection.format)}</span>
              </span>
              <span className="text-zinc-500">
                {inspection.rowCount.toLocaleString()} row
                {inspection.rowCount === 1 ? "" : "s"}
              </span>
              <span className="text-zinc-500">·</span>
              <span className="text-zinc-500">
                {inspection.chunkCount.toLocaleString()}{" "}
                {chunkUnitLabel(inspection.format, inspection.chunkCount)}
              </span>
              {inspection.compression && (
                <>
                  <span className="text-zinc-500">·</span>
                  <span className="text-zinc-500">codec {inspection.compression}</span>
                </>
              )}
              <span className="text-zinc-500">·</span>
              <span className="text-zinc-500">{formatBytes(inspection.fileSize)} on disk</span>
              <span className="text-zinc-500">·</span>
              <span className="text-zinc-500">
                {inspection.columns.length.toLocaleString()} column
                {inspection.columns.length === 1 ? "" : "s"}
              </span>
            </div>

            {isArrow && (
              <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
                Arrow IPC file is also known as <span className="font-medium">Feather v2</span> —
                the same container.
              </p>
            )}

            {/* Schema table with indented nesting */}
            <Section label="Schema">
              <div className="max-h-64 overflow-auto rounded border border-zinc-200 dark:border-zinc-800">
                <table className="w-full border-collapse text-[11px]">
                  <thead className="sticky top-0 bg-white text-left uppercase tracking-wide text-zinc-400 dark:bg-zinc-900">
                    <tr>
                      <th className="px-2 py-1 font-medium">Column</th>
                      <th className="px-2 py-1 font-medium">Logical type</th>
                      <th className="px-2 py-1 font-medium">Arrow type</th>
                      <th className="px-2 py-1 font-medium">Null</th>
                    </tr>
                  </thead>
                  <tbody>
                    {shownColumns.map((c) => {
                      const depth = columnDepth(c.name);
                      return (
                        <tr key={c.name} className="border-t border-zinc-100 dark:border-zinc-800">
                          <td
                            className="max-w-64 truncate px-2 py-1 font-mono"
                            title={c.name}
                            style={{ paddingLeft: `${0.5 + depth * 1}rem` }}
                          >
                            {depth > 0 && <span className="text-zinc-400">↳ </span>}
                            {depth > 0 ? leafName(c.name) : c.name}
                            {c.nested && (
                              <span className="ml-1 rounded bg-zinc-100 px-1 text-[9px] text-zinc-500 dark:bg-zinc-800">
                                nested
                              </span>
                            )}
                          </td>
                          <td className="px-2 py-1">
                            {LOGICAL_TYPE_LABELS[c.logicalType]}
                            {c.timeZone && (
                              <span className="ml-1 text-zinc-400" title="Preserved timezone">
                                ({c.timeZone})
                              </span>
                            )}
                          </td>
                          <td
                            className="max-w-56 truncate px-2 py-1 font-mono text-zinc-500"
                            title={c.arrowType}
                          >
                            {c.arrowType}
                          </td>
                          <td className="px-2 py-1 text-zinc-400">{c.nullable ? "yes" : "—"}</td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
              {hiddenColumns > 0 && (
                <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
                  + {hiddenColumns.toLocaleString()} more column
                  {hiddenColumns === 1 ? "" : "s"} not shown (all open).
                </p>
              )}
            </Section>

            {/* Complex-field policies */}
            {inspection.complexFields.length > 0 && (
              <Section label="Nested list / map / struct fields">
                <div className="mb-1.5 flex items-center gap-2 text-xs">
                  <span className="text-zinc-500">Default policy</span>
                  <Segmented
                    value={options.complexPolicy ?? "preserveJson"}
                    options={[
                      { value: "preserveJson", label: "Keep as JSON" },
                      { value: "reject", label: "Drop" },
                    ]}
                    onChange={(v) => setDefaultPolicy(v as ComplexPolicy)}
                  />
                </div>
                <div className="max-h-40 space-y-1 overflow-y-auto">
                  {inspection.complexFields.map((path) => (
                    <div key={path} className="flex items-center gap-2 text-xs">
                      <span className="max-w-56 truncate font-mono text-[11px]" title={path}>
                        {path}
                      </span>
                      <select
                        value={effectivePolicy(options, path)}
                        onChange={(e) => setPolicy(path, e.target.value as ComplexPolicy)}
                        className={`${selectCls} ml-auto`}
                      >
                        <option value="preserveJson">Keep as JSON</option>
                        <option value="explode">Explode into rows</option>
                        <option value="reject">Drop field</option>
                      </select>
                    </div>
                  ))}
                </div>
                <p className="mt-1 text-[11px] text-zinc-500 dark:text-zinc-400">
                  Exploding a list multiplies the record into one row per element — editable open
                  only, and one field at a time.
                </p>
              </Section>
            )}

            {/* Open-plan errors / editable-only notice */}
            {plan?.errors.map((e, i) => (
              <p key={i} className="text-xs text-red-600 dark:text-red-400">
                {e}
              </p>
            ))}
            {plan?.requiresEditable && plan.errors.length === 0 && (
              <p className="text-xs text-amber-600 dark:text-amber-400">
                {plan.indexedDisabledReason}
              </p>
            )}

            {/* Memory estimate */}
            <div className="rounded border border-zinc-200 px-3 py-2 text-xs dark:border-zinc-800">
              <div className="flex items-center justify-between">
                <span className="text-zinc-500 dark:text-zinc-400">
                  Estimated memory if fully editable
                </span>
                <span className="tabular-nums">~{estimatedMemoryLabel(inspection)}</span>
              </div>
              {needsDecision ? (
                <p className="mt-1 text-amber-600 dark:text-amber-400">
                  This is large — read-only (indexed) keeps memory bounded and still supports
                  browsing, find, filter, export and profiling. Converting to editable loads it all
                  into memory.
                </p>
              ) : (
                <p className="mt-1 text-zinc-500 dark:text-zinc-400">
                  Read-only (indexed) streams rows on demand; convert to editable to change cells.
                  Either way the source file is never written over — an edited copy saves to a new
                  destination.
                </p>
              )}
              {needsDecision && ackEditable && (
                <p className="mt-1 text-amber-600 dark:text-amber-400">
                  Opening editable may exhaust memory. Click again to proceed anyway.
                </p>
              )}
            </div>
          </>
        )}
      </div>
    </Modal>
  );
}

function Section({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="space-y-1">
      <p className="text-xs font-medium text-zinc-600 dark:text-zinc-300">{label}</p>
      {children}
    </div>
  );
}

function Segmented({
  value,
  options,
  onChange,
}: {
  value: string;
  options: { value: string; label: string }[];
  onChange: (value: string) => void;
}) {
  return (
    <div className="flex overflow-hidden rounded border border-zinc-300 text-xs dark:border-zinc-700">
      {options.map((o) => (
        <button
          key={o.value}
          onClick={() => onChange(o.value)}
          className={`px-2.5 py-1 ${value === o.value ? "bg-violet-600 text-white" : "text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"}`}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const selectCls =
  "rounded border border-zinc-300 bg-transparent px-2 py-1 text-xs outline-none focus:border-violet-500 dark:border-zinc-700";
