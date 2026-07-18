import { useEffect, useMemo, useState } from "react";

import {
  canApplyImport,
  describeShape,
  isIgnored,
  needsMultiArrayChoice,
  previewReflectsOptions,
  toggleIgnorePath,
  validateImportOptions,
} from "../lib/jsonImport";
import { useStore } from "../store/useStore";
import type { ArrayPolicy, JsonImportOptions, MultiArrayMode, NestedPolicy } from "../types";
import { Modal } from "./Modal";

/**
 * Upper bound on how many columns the preview tables render at once. The
 * import itself is unaffected (the backend allows up to 10,000 columns); this
 * only keeps the dialog's DOM bounded, per the "bounded windows to React only"
 * invariant. A wide import shows the first slice with a "+N more" affordance.
 */
const MAX_PREVIEW_COLUMNS = 200;

/**
 * JSON / JSON Lines import preview + policy dialog (F33). Self-driven by the
 * `jsonImport` store slice, so opening a `.json` / `.jsonl` / `.ndjson` file
 * (or the "Open JSON…" command) shows it automatically. Every option change
 * re-runs a full-pass preview scan through the job registry; the import itself
 * runs as a cancellable derive job into a NEW document.
 */
export function JsonImportDialog() {
  const st = useStore((s) => s.jsonImport);
  const derive = useStore((s) => s.derive);
  const deriveError = useStore((s) => s.deriveError);
  const runJsonScan = useStore((s) => s.runJsonScan);
  const applyJsonImport = useStore((s) => s.applyJsonImport);
  const cancelJsonScan = useStore((s) => s.cancelJsonScan);
  const cancelDerive = useStore((s) => s.cancelDerive);
  const dismiss = useStore((s) => s.dismissJsonImport);

  // The options being edited; the store re-scans whenever they diverge from the
  // options the current preview was produced under.
  const [opts, setOpts] = useState<JsonImportOptions>(() =>
    st ? { ...st.options } : ({} as JsonImportOptions),
  );

  const optionsKey = JSON.stringify(opts);
  const scannedKey = st ? JSON.stringify(st.options) : optionsKey;

  useEffect(() => {
    if (optionsKey === scannedKey) return; // preview already reflects these options
    const timer = setTimeout(
      () => void runJsonScan(JSON.parse(optionsKey) as JsonImportOptions),
      300,
    );
    return () => clearTimeout(timer);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [optionsKey, scannedKey]);

  const preview = st?.preview ?? null;
  const importing = derive?.kind === "jsonImport";
  const scanning = st?.scanJobId != null;

  const errors = useMemo(
    () => (st ? validateImportOptions(opts, preview) : []),
    [st, opts, preview],
  );

  if (!st) return null;
  const patch = (p: Partial<JsonImportOptions>) => setOpts((o) => ({ ...o, ...p }));

  const showPointer =
    preview != null &&
    (preview.shape === "objectDocument" || preview.needsPointer || preview.candidates.length > 0);
  const hasColumns = (preview?.columns.length ?? 0) > 0;
  const explodeActive = opts.arrayPolicy === "explode";
  const needsMulti = preview ? needsMultiArrayChoice(preview, opts) : false;

  // Keep the preview tables bounded (the invariant is "bounded windows to
  // React only"): the backend allows up to 10,000 flattened columns, which
  // would otherwise render as hundreds of thousands of DOM nodes here.
  const shownColumns = preview ? preview.columns.slice(0, MAX_PREVIEW_COLUMNS) : [];
  const hiddenColumns = (preview?.columns.length ?? 0) - shownColumns.length;

  // Only enable Import when the shown preview reflects the edited options (a
  // just-changed option is still within the debounce / rescan, so the preview
  // is stale) and the last scan succeeded — otherwise the applied options would
  // not match what the preview shows.
  const previewIsCurrent = previewReflectsOptions(opts, st.options);
  const canImport = canApplyImport({
    importing,
    scanning,
    scanError: st.scanError,
    errors,
    hasColumns,
    editedOptions: opts,
    scannedOptions: st.options,
  });

  return (
    <Modal
      title={`Import JSON — ${st.fileName}`}
      onClose={dismiss}
      size="xl"
      footer={
        <>
          <button onClick={dismiss} className={btnGhost}>
            Cancel
          </button>
          <button
            onClick={() => void applyJsonImport(opts)}
            disabled={!canImport}
            title={
              errors[0] ??
              (!hasColumns
                ? "Nothing to import yet"
                : scanning || !previewIsCurrent
                  ? "Re-scanning with the changed options…"
                  : undefined)
            }
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            {importing ? "Importing…" : "Import into a new document"}
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        {/* Shape summary */}
        <div className="flex flex-wrap items-center gap-x-4 gap-y-1 rounded bg-zinc-50 px-3 py-2 text-xs dark:bg-zinc-900">
          <span>
            Shape:{" "}
            <span className="font-medium">{preview ? describeShape(preview.shape) : "…"}</span>
          </span>
          {preview?.recordKind && (
            <span className="text-zinc-500">records: {preview.recordKind}</span>
          )}
          {preview && !preview.needsPointer && (
            <>
              <span className="text-zinc-500">
                {preview.recordCount.toLocaleString()} record
                {preview.recordCount === 1 ? "" : "s"}
              </span>
              <span className="text-zinc-500">→</span>
              <span className="font-medium tabular-nums">
                {preview.projectedRows.toLocaleString()} row
                {preview.projectedRows === 1 ? "" : "s"} × {preview.projectedColumns} column
                {preview.projectedColumns === 1 ? "" : "s"}
              </span>
              {preview.exploded && (
                <span className="text-amber-600 dark:text-amber-400">exploded</span>
              )}
            </>
          )}
        </div>

        {/* Record-array pointer picker */}
        {showPointer && preview && (
          <Section label="Record-array location (JSON Pointer)">
            {preview.needsPointer && (
              <p className="mb-1 text-xs text-amber-600 dark:text-amber-400">
                This object document has no top-level record array — pick where the records live.
              </p>
            )}
            {preview.candidates.length > 0 && (
              <div className="mb-1.5 max-h-32 space-y-0.5 overflow-y-auto">
                {preview.candidates.map((c) => (
                  <label key={c.pointer} className="flex items-center gap-2">
                    <input
                      type="radio"
                      name="json-pointer"
                      checked={(opts.pointer ?? "") === c.pointer}
                      onChange={() => patch({ pointer: c.pointer })}
                      className="accent-violet-600"
                    />
                    <span className="font-mono text-[11px]">
                      {c.pointer === "" ? "/ (root)" : c.pointer}
                    </span>
                    <span className="text-zinc-400">
                      {c.records.toLocaleString()} × {c.elementKind}
                    </span>
                  </label>
                ))}
              </div>
            )}
            <label className="flex items-center gap-2 text-xs">
              <span className="text-zinc-500">Pointer</span>
              <input
                type="text"
                value={opts.pointer ?? ""}
                placeholder="/data/items"
                onChange={(e) => patch({ pointer: e.target.value })}
                className={`${inputCls} w-64 font-mono`}
              />
            </label>
          </Section>
        )}

        {/* Missing vs explicit null */}
        <Section label="Missing vs. explicit null">
          <div className="flex flex-wrap gap-4 text-xs">
            <label className="flex items-center gap-2">
              <span className="text-zinc-500">Explicit null →</span>
              <input
                type="text"
                value={opts.nullToken}
                onChange={(e) => patch({ nullToken: e.target.value })}
                className={`${inputCls} w-28`}
              />
            </label>
            <label className="flex items-center gap-2">
              <span className="text-zinc-500">Missing field →</span>
              <input
                type="text"
                value={opts.missingToken}
                placeholder="(empty)"
                onChange={(e) => patch({ missingToken: e.target.value })}
                className={`${inputCls} w-28`}
              />
            </label>
          </div>
          <p className="mt-1 text-[11px] text-zinc-500 dark:text-zinc-400">
            A JSON <code>null</code> becomes the null token; a property absent from a record becomes
            the missing token. The two must differ so the distinction survives round-trips.
          </p>
        </Section>

        {hasColumns && (
          <>
            {/* Nested-object policy */}
            <Section label="Nested objects">
              <Segmented
                value={opts.nestedPolicy}
                options={[
                  { value: "flatten", label: "Flatten to path columns" },
                  { value: "preserveJson", label: "Keep as compact JSON" },
                ]}
                onChange={(v) => patch({ nestedPolicy: v as NestedPolicy })}
              />
              {preview!.nestedObjectPaths.length > 0 && (
                <div className="mt-1.5 max-h-28 space-y-0.5 overflow-y-auto text-xs">
                  <p className="text-zinc-500 dark:text-zinc-400">
                    Ignore selected object paths (drops them and everything under them):
                  </p>
                  {preview!.nestedObjectPaths.map((path) => (
                    <label key={path} className="flex items-center gap-2">
                      <input
                        type="checkbox"
                        checked={isIgnored(path, opts.ignorePaths)}
                        onChange={() =>
                          patch({ ignorePaths: toggleIgnorePath(opts.ignorePaths, path) })
                        }
                        className="accent-violet-600"
                      />
                      <span className="font-mono text-[11px]">{path}</span>
                    </label>
                  ))}
                </div>
              )}
            </Section>

            {/* Array policy */}
            <Section label="Array-valued fields">
              <div className="flex items-center gap-2">
                <select
                  value={opts.arrayPolicy}
                  onChange={(e) => patch({ arrayPolicy: e.target.value as ArrayPolicy })}
                  className={selectCls}
                >
                  <option value="preserveJson">Keep as JSON</option>
                  <option value="explode">Explode into rows</option>
                  <option value="join">Join primitives with…</option>
                  <option value="reject">Reject (fail if any array)</option>
                </select>
                {opts.arrayPolicy === "join" && (
                  <input
                    type="text"
                    value={opts.joinSeparator ?? ""}
                    onChange={(e) => patch({ joinSeparator: e.target.value })}
                    className={`${inputCls} w-20`}
                    placeholder="sep"
                  />
                )}
              </div>

              {explodeActive && needsMulti && (
                <div className="mt-1.5">
                  <p className="text-xs text-amber-600 dark:text-amber-400">
                    Two or more array fields explode — choose how to combine them:
                  </p>
                  <Segmented
                    value={opts.multiArray ?? ""}
                    options={[
                      { value: "cartesian", label: "Cartesian (all combinations)" },
                      { value: "zip", label: "Zip (pair by index)" },
                    ]}
                    onChange={(v) => patch({ multiArray: v as MultiArrayMode })}
                  />
                </div>
              )}
              {explodeActive && !needsMulti && opts.multiArray && (
                <p className="mt-1 text-[11px] text-zinc-500">
                  Combine mode: {opts.multiArray} (only used when 2+ fields explode).
                </p>
              )}

              {preview!.arrayFields.length > 0 && (
                <div className="mt-1.5 max-h-28 space-y-0.5 overflow-y-auto text-xs">
                  {preview!.arrayFields.map((f) => (
                    <div key={f.path} className="flex items-center gap-2">
                      <span className="font-mono text-[11px]">{f.path}</span>
                      <span className="text-zinc-400">
                        ≤{f.maxLen} items · {f.occurrences.toLocaleString()}×
                        {f.primitivesOnly ? " · primitives" : " · has objects"}
                      </span>
                      <label className="ml-auto flex items-center gap-1 text-zinc-500">
                        <input
                          type="checkbox"
                          checked={isIgnored(f.path, opts.ignorePaths)}
                          onChange={() =>
                            patch({ ignorePaths: toggleIgnorePath(opts.ignorePaths, f.path) })
                          }
                          className="accent-violet-600"
                        />
                        ignore
                      </label>
                    </div>
                  ))}
                </div>
              )}
            </Section>

            {/* Per-column missing/null counts */}
            <Section label="Columns (present / null / missing)">
              <div className="max-h-40 overflow-auto rounded border border-zinc-200 dark:border-zinc-800">
                <table className="w-full border-collapse text-[11px]">
                  <thead className="sticky top-0 bg-white text-left uppercase tracking-wide text-zinc-400 dark:bg-zinc-900">
                    <tr>
                      <th className="px-2 py-1 font-medium">Path</th>
                      <th className="px-2 py-1 font-medium">Type</th>
                      <th className="px-2 py-1 text-right font-medium">Present</th>
                      <th className="px-2 py-1 text-right font-medium">Null</th>
                      <th className="px-2 py-1 text-right font-medium">Missing</th>
                    </tr>
                  </thead>
                  <tbody>
                    {shownColumns.map((c) => (
                      <tr key={c.name} className="border-t border-zinc-100 dark:border-zinc-800">
                        <td className="max-w-64 truncate px-2 py-1 font-mono" title={c.name}>
                          {c.name}
                        </td>
                        <td className="px-2 py-1 text-zinc-500">{c.inferredType}</td>
                        <td className="px-2 py-1 text-right tabular-nums">
                          {c.present.toLocaleString()}
                        </td>
                        <td className="px-2 py-1 text-right tabular-nums text-sky-600 dark:text-sky-400">
                          {c.nulls.toLocaleString()}
                        </td>
                        <td className="px-2 py-1 text-right tabular-nums text-amber-600 dark:text-amber-400">
                          {c.missing.toLocaleString()}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
              {hiddenColumns > 0 && (
                <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
                  + {hiddenColumns.toLocaleString()} more column
                  {hiddenColumns === 1 ? "" : "s"} not shown (all{" "}
                  {preview!.projectedColumns.toLocaleString()} import).
                </p>
              )}
            </Section>

            {/* Projected preview grid */}
            {preview!.sampleRows.length > 0 && (
              <Section label={`Preview (${preview!.sampleRows.length} sample rows)`}>
                <div className="max-h-48 overflow-auto rounded border border-zinc-200 dark:border-zinc-800">
                  <table className="border-collapse text-[11px]">
                    <thead className="sticky top-0 bg-white text-left text-zinc-400 dark:bg-zinc-900">
                      <tr>
                        {shownColumns.map((c) => (
                          <th
                            key={c.name}
                            className="whitespace-nowrap border-b border-zinc-200 px-2 py-1 font-mono font-medium dark:border-zinc-800"
                          >
                            {c.name}
                          </th>
                        ))}
                        {hiddenColumns > 0 && (
                          <th className="whitespace-nowrap border-b border-zinc-200 px-2 py-1 font-medium text-zinc-400 dark:border-zinc-800">
                            +{hiddenColumns.toLocaleString()}…
                          </th>
                        )}
                      </tr>
                    </thead>
                    <tbody>
                      {preview!.sampleRows.map((row, ri) => (
                        <tr key={ri} className="border-t border-zinc-100 dark:border-zinc-900">
                          {row.slice(0, MAX_PREVIEW_COLUMNS).map((cell, ci) => (
                            <td
                              key={ci}
                              className="max-w-64 truncate px-2 py-1 font-mono"
                              title={cell}
                            >
                              {cell}
                            </td>
                          ))}
                          {hiddenColumns > 0 && <td className="px-2 py-1 text-zinc-400">…</td>}
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              </Section>
            )}
          </>
        )}

        {/* Backing mode */}
        <label className="flex items-center gap-2 text-xs">
          <input
            type="checkbox"
            checked={opts.forceIndexed}
            onChange={(e) => patch({ forceIndexed: e.target.checked })}
            className="accent-violet-600"
          />
          Open read-only (indexed) — bounded memory for large inputs
        </label>

        {/* Warnings */}
        {preview && preview.warnings.length > 0 && (
          <ul className="space-y-0.5 text-xs text-amber-600 dark:text-amber-400">
            {preview.warnings.map((w, i) => (
              <li key={i}>• {w}</li>
            ))}
          </ul>
        )}

        {/* Errors */}
        {errors.length > 0 && (
          <ul className="space-y-0.5 text-xs text-red-600 dark:text-red-400">
            {errors.map((e, i) => (
              <li key={i}>• {e}</li>
            ))}
          </ul>
        )}
        {(st.scanError ?? deriveError) && (
          <p className="text-xs text-red-600 dark:text-red-400">{st.scanError ?? deriveError}</p>
        )}

        {/* Progress */}
        {scanning && (
          <div className="flex items-center gap-3 text-xs text-zinc-500 dark:text-zinc-400">
            <span>
              Scanning…
              {st.scanTotal != null &&
                st.scanTotal > 0 &&
                ` ${Math.min(100, Math.round((st.scanProcessed / st.scanTotal) * 100))}%`}
            </span>
            <button onClick={() => void cancelJsonScan()} className={cancelBtn}>
              Cancel scan
            </button>
          </div>
        )}
        {importing && derive && (
          <div className="flex items-center gap-3 text-xs text-zinc-500 dark:text-zinc-400">
            <span>
              {derive.message ?? "importing"} — {derive.processed.toLocaleString()}
              {derive.total != null && ` / ${derive.total.toLocaleString()}`}
            </span>
            <button onClick={() => void cancelDerive()} className={cancelBtn}>
              Cancel
            </button>
          </div>
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
    <div className="flex flex-wrap overflow-hidden rounded border border-zinc-300 text-xs dark:border-zinc-700">
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
  "rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700";
const inputCls =
  "rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700";
const cancelBtn =
  "rounded px-2 py-1 text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10";
