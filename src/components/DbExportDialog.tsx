import {
  SQL_TYPES,
  canRunExport,
  columnCompatibility,
  describeConflict,
  exportBlockers,
  type ColumnOverride,
} from "../lib/dbExport";
import { useStore } from "../store/useStore";
import type { DbConflictPolicy, DbExportMode, DbSqlType } from "../types";
import { Modal } from "./Modal";

/**
 * Database export dialog (F35). Writes the active document into a SQLite table —
 * create a new table, append to a compatible existing one, or replace one after
 * explicit confirmation. Every option re-runs a preview: the resolved column →
 * SQL-type mapping, any append incompatibilities, and a bounded scan of the
 * cells that would FAIL to convert, all shown BEFORE the (single-transaction,
 * roll-back-on-failure) write runs. Driven entirely by the `dbExport` store
 * slice. SQLite only this cycle.
 */
export function DbExportDialog() {
  const st = useStore((s) => s.dbExport);
  const close = useStore((s) => s.closeDbExport);
  const patch = useStore((s) => s.patchDbExportForm);
  const chooseTarget = useStore((s) => s.chooseDbExportTarget);
  const run = useStore((s) => s.runDbExport);
  const cancel = useStore((s) => s.cancelDbExport);

  if (!st) return null;
  const { form, preview } = st;
  const running = st.jobId != null;
  const blockers = exportBlockers(form, preview);
  const runnable = canRunExport(form, preview) && !running;
  const appendMode = form.mode === "append";

  const setOverride = (columnId: string, over: Partial<ColumnOverride>) => {
    const cur = form.overrides[columnId] ?? {};
    patch({ overrides: { ...form.overrides, [columnId]: { ...cur, ...over } } });
  };

  return (
    <Modal
      title={`Export to database — ${st.docName}`}
      onClose={close}
      size="2xl"
      footer={
        <>
          <button onClick={close} className={btnGhost}>
            {st.result ? "Close" : "Cancel"}
          </button>
          {running ? (
            <button onClick={() => void cancel()} className={cancelBtn}>
              Cancel write
            </button>
          ) : (
            <button
              onClick={() => void run()}
              disabled={!runnable}
              title={blockers[0]}
              className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
            >
              Export
            </button>
          )}
        </>
      }
    >
      <div className="space-y-3 text-sm">
        {/* Success report */}
        {st.result && (
          <div className="rounded border border-emerald-300 bg-emerald-50 px-3 py-2 text-xs text-emerald-800 dark:border-emerald-500/40 dark:bg-emerald-950/40 dark:text-emerald-300">
            Wrote {st.result.rowsWritten.toLocaleString()} row
            {st.result.rowsWritten === 1 ? "" : "s"} to{" "}
            <span className="font-mono">{st.result.table}</span> ({st.result.mode})
            {st.result.rowsSkipped > 0 && ` — ${st.result.rowsSkipped.toLocaleString()} skipped`}.
          </div>
        )}

        {/* Target */}
        <Section label="Target database">
          <div className="flex items-center gap-2">
            <span
              className={`min-w-0 flex-1 truncate rounded border px-2 py-1 font-mono text-xs ${
                form.path
                  ? "border-zinc-300 dark:border-zinc-700"
                  : "border-dashed border-zinc-300 text-zinc-400 dark:border-zinc-700"
              }`}
              title={form.path ?? undefined}
            >
              {form.path ?? "No database chosen"}
            </span>
            <button onClick={() => void chooseTarget(false)} className={btnSecondary}>
              Choose existing…
            </button>
            <button onClick={() => void chooseTarget(true)} className={btnSecondary}>
              New file…
            </button>
          </div>
        </Section>

        {/* Table + mode */}
        <div className="flex flex-wrap items-end gap-4">
          <label className="space-y-1">
            <span className="block text-xs font-medium text-zinc-600 dark:text-zinc-300">
              Table name
            </span>
            <input
              type="text"
              value={form.table}
              onChange={(e) => patch({ table: e.target.value })}
              className={`${inputCls} w-56 font-mono`}
            />
          </label>
          <div className="space-y-1">
            <span className="block text-xs font-medium text-zinc-600 dark:text-zinc-300">Mode</span>
            <Segmented
              value={form.mode}
              options={[
                { value: "create", label: "Create" },
                { value: "append", label: "Append" },
                { value: "replace", label: "Replace" },
              ]}
              onChange={(v) => patch({ mode: v as DbExportMode })}
            />
          </div>
        </div>

        {/* Replace confirmation */}
        {form.mode === "replace" && preview?.tableExists && (
          <label className="flex items-start gap-2 rounded border border-red-300 bg-red-50 px-3 py-2 text-xs text-red-700 dark:border-red-500/40 dark:bg-red-950/40 dark:text-red-300">
            <input
              type="checkbox"
              checked={form.confirmReplace}
              onChange={(e) => patch({ confirmReplace: e.target.checked })}
              className="mt-0.5 accent-red-600"
            />
            <span>
              Drop and recreate <span className="font-mono">{form.table}</span>, discarding its
              existing {(preview.targetRows ?? 0).toLocaleString()} row
              {preview.targetRows === 1 ? "" : "s"}. This cannot be undone.
            </span>
          </label>
        )}

        {/* Conflict policy */}
        <label className="flex items-center gap-2 text-xs">
          <span className="text-zinc-500">On primary-key / unique conflict</span>
          <select
            value={form.conflictPolicy}
            onChange={(e) => patch({ conflictPolicy: e.target.value as DbConflictPolicy })}
            className={selectCls}
          >
            {(["abort", "skip", "replace"] as DbConflictPolicy[]).map((p) => (
              <option key={p} value={p}>
                {describeConflict(p)}
              </option>
            ))}
          </select>
        </label>

        {/* Mapping editor */}
        <Section
          label={
            appendMode
              ? "Column mapping (append: the existing table's types apply)"
              : "Column mapping (document → SQL)"
          }
        >
          {st.previewLoading && !preview ? (
            <p className="text-xs text-zinc-500">Preparing preview…</p>
          ) : preview ? (
            <div className="max-h-56 overflow-auto rounded border border-zinc-200 dark:border-zinc-800">
              <table className="w-full border-collapse text-[11px]">
                <thead className="sticky top-0 bg-zinc-50 text-left uppercase tracking-wide text-zinc-400 dark:bg-zinc-900">
                  <tr>
                    <th className="px-2 py-1 font-medium">Column</th>
                    <th className="px-2 py-1 font-medium">SQL name</th>
                    <th className="px-2 py-1 font-medium">SQL type</th>
                    {!appendMode && <th className="px-2 py-1 font-medium">PK</th>}
                    <th className="px-2 py-1 font-medium">{appendMode ? "Target" : ""}</th>
                  </tr>
                </thead>
                <tbody>
                  {preview.columns.map((c) => {
                    const ov = form.overrides[c.columnId] ?? {};
                    const compat = columnCompatibility(c, form.mode);
                    return (
                      <tr
                        key={c.columnId}
                        className="border-t border-zinc-100 dark:border-zinc-800"
                      >
                        <td className="px-2 py-1 font-mono text-zinc-500" title={c.name}>
                          {c.name}
                        </td>
                        <td className="px-2 py-1">
                          <input
                            type="text"
                            value={ov.sqlName ?? ""}
                            placeholder={c.sqlName}
                            onChange={(e) => setOverride(c.columnId, { sqlName: e.target.value })}
                            className={`${inputCls} w-36 font-mono text-[11px]`}
                          />
                        </td>
                        <td className="px-2 py-1">
                          <select
                            value={ov.sqlType ?? c.sqlType}
                            disabled={appendMode}
                            onChange={(e) =>
                              setOverride(c.columnId, { sqlType: e.target.value as DbSqlType })
                            }
                            className={`${selectCls} disabled:opacity-60`}
                          >
                            {SQL_TYPES.map((t) => (
                              <option key={t} value={t}>
                                {t}
                              </option>
                            ))}
                          </select>
                        </td>
                        {!appendMode && (
                          <td className="px-2 py-1 text-center">
                            <input
                              type="checkbox"
                              checked={ov.primaryKey ?? c.primaryKey}
                              onChange={(e) =>
                                setOverride(c.columnId, { primaryKey: e.target.checked })
                              }
                              className="accent-violet-600"
                            />
                          </td>
                        )}
                        <td
                          className={`px-2 py-1 ${compat.ok ? "text-zinc-400" : "text-red-600 dark:text-red-400"}`}
                        >
                          {appendMode ? compat.label : ""}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          ) : (
            <p className="text-xs text-zinc-500">No preview yet.</p>
          )}
        </Section>

        {/* Conversion failures */}
        {preview && preview.failureCount > 0 && (
          <Section
            label={`Conversion failures — ${preview.failureCount.toLocaleString()}${
              preview.scanComplete ? "" : "+"
            } cell${preview.failureCount === 1 ? "" : "s"} would abort the write`}
          >
            <div className="max-h-32 overflow-auto rounded border border-red-200 dark:border-red-900/50">
              <table className="w-full border-collapse text-[11px]">
                <thead className="sticky top-0 bg-red-50 text-left text-red-500 dark:bg-red-950/40">
                  <tr>
                    <th className="px-2 py-1 font-medium">Row</th>
                    <th className="px-2 py-1 font-medium">Column</th>
                    <th className="px-2 py-1 font-medium">Value</th>
                    <th className="px-2 py-1 font-medium">Reason</th>
                  </tr>
                </thead>
                <tbody>
                  {preview.failures.map((f, i) => (
                    <tr key={i} className="border-t border-red-100 dark:border-red-900/40">
                      <td className="px-2 py-1 tabular-nums">{(f.row + 1).toLocaleString()}</td>
                      <td className="px-2 py-1 font-mono">{f.column}</td>
                      <td className="max-w-40 truncate px-2 py-1 font-mono" title={f.value}>
                        {f.value}
                      </td>
                      <td className="px-2 py-1 text-zinc-500">{f.reason}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <p className="mt-1 text-[11px] text-zinc-500">
              Map the affected columns to TEXT, or fix the data, before exporting.
            </p>
          </Section>
        )}

        {/* Blocking issues (append incompatibilities, etc.) */}
        {preview && preview.blocking.length > 0 && (
          <ul className="space-y-0.5 text-xs text-red-600 dark:text-red-400">
            {preview.blocking.map((b, i) => (
              <li key={i}>• {b}</li>
            ))}
          </ul>
        )}

        {/* Preview / job errors */}
        {st.previewError && (
          <p className="text-xs text-red-600 dark:text-red-400">{st.previewError}</p>
        )}
        {st.error && <p className="text-xs text-red-600 dark:text-red-400">{st.error}</p>}

        {/* Write progress */}
        {running && (
          <div className="flex items-center gap-3 text-xs text-zinc-500 dark:text-zinc-400">
            <span>
              Writing… {st.processed.toLocaleString()}
              {st.total != null && ` / ${st.total.toLocaleString()}`} rows
            </span>
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
    <div className="flex overflow-hidden rounded border border-zinc-300 text-xs dark:border-zinc-700">
      {options.map((o) => (
        <button
          key={o.value}
          onClick={() => onChange(o.value)}
          className={`px-2.5 py-1 ${
            value === o.value
              ? "bg-violet-600 text-white"
              : "text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"
          }`}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
const btnSecondary =
  "rounded border border-zinc-300 px-2.5 py-1 text-xs text-zinc-700 hover:bg-zinc-100 dark:border-zinc-700 dark:text-zinc-200 dark:hover:bg-zinc-800";
const selectCls =
  "rounded border border-zinc-300 bg-transparent px-2 py-1 text-xs outline-none focus:border-violet-500 dark:border-zinc-700";
const inputCls =
  "rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700";
const cancelBtn =
  "rounded px-3 py-1.5 text-sm text-red-600 hover:bg-red-50 dark:text-red-400 dark:hover:bg-red-500/10";
