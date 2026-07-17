import { useMemo, useState } from "react";

import { scopeChoices } from "../lib/export";
import { buildRebuildMapping, defaultJsonExportOptions, jsonFormatLabel } from "../lib/jsonExport";
import { useActiveMeta, useStore } from "../store/useStore";
import type { ExportScope, JsonExportFormat, JsonExportOptions } from "../types";
import { Modal } from "./Modal";

const FORMATS: JsonExportFormat[] = ["objects", "arrays", "jsonLines"];

/**
 * Upper bound on how many rebuild-mapping rows the dialog renders. Conflict
 * detection still runs over EVERY column; this only bounds the DOM, per the
 * "bounded windows to React only" invariant (a JSON-imported document can be
 * thousands of columns wide).
 */
const MAX_MAPPING_ROWS = 200;

/**
 * JSON / JSON Lines export (F33). Format choice, missing/null token mapping,
 * typed emission, and — for the object formats — a nested-rebuild mapping that
 * surfaces duplicate / conflicting output paths BEFORE the write is attempted
 * (the backend re-checks and rejects too). Exports never touch the document's
 * save point.
 */
export function JsonExportDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const exportJson = useStore((s) => s.exportJson);
  const filtered = useStore((s) => s.tabs.find((t) => t.id === s.activeId)?.filtered ?? false);
  const selectedRows = useStore((s) => s.selectedRows);
  const selectedCols = useStore((s) => s.selectedCols);
  const selectionRect = useStore((s) => s.selectionPhysicalRect)();
  const viewSorted = meta?.viewSorted ?? false;

  const [opts, setOpts] = useState<JsonExportOptions>(() => defaultJsonExportOptions());

  const choices = useMemo(
    () => scopeChoices(filtered, selectionRect, selectedRows, selectedCols, viewSorted),
    [filtered, selectionRect, selectedRows, selectedCols, viewSorted],
  );
  const [scopeIdx, setScopeIdx] = useState(0);
  const scope: ExportScope = (choices[scopeIdx] ?? choices[0]).scope;

  // The header names that will actually be written, for the rebuild mapping.
  const exportedHeaders = useMemo(() => {
    if (!meta) return [];
    if (scope.type === "selectedColumns") {
      return scope.columns.map((i) => meta.headers[i] ?? `Column ${i + 1}`);
    }
    if (scope.type === "selectedRange") {
      const out: string[] = [];
      for (let i = scope.rect.x; i < scope.rect.x + scope.rect.width; i++) {
        out.push(meta.headers[i] ?? `Column ${i + 1}`);
      }
      return out;
    }
    return meta.headers;
  }, [meta, scope]);

  const isArrays = opts.format === "arrays";
  const rebuild = opts.rebuildNested && !isArrays;
  const mapping = useMemo(
    () => (isArrays ? null : buildRebuildMapping(exportedHeaders, rebuild)),
    [exportedHeaders, rebuild, isArrays],
  );
  const conflict = mapping?.conflict ?? null;
  const tokensClash = (opts.nullToken ?? null) !== null && opts.nullToken === opts.missingToken;

  if (!meta) return null;
  const patch = (p: Partial<JsonExportOptions>) => setOpts((o) => ({ ...o, ...p }));

  const blocked = conflict !== null || tokensClash;

  const doExport = () => {
    if (blocked) return;
    void exportJson(opts, scope);
    onClose();
  };

  return (
    <Modal
      title="Export as JSON"
      onClose={onClose}
      size="lg"
      footer={
        <>
          <button onClick={onClose} className={btnGhost}>
            Cancel
          </button>
          <button
            onClick={doExport}
            disabled={blocked}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            Choose file & export
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <Row label="Format">
          <select
            value={opts.format}
            onChange={(e) => patch({ format: e.target.value as JsonExportFormat })}
            className={selectCls}
          >
            {FORMATS.map((f) => (
              <option key={f} value={f} className="dark:bg-zinc-800">
                {jsonFormatLabel(f)}
              </option>
            ))}
          </select>
        </Row>

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

        <div className="flex flex-wrap gap-x-5 gap-y-2">
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={opts.typed}
              onChange={(e) => patch({ typed: e.target.checked })}
              className="accent-violet-600"
            />
            Typed values (numbers, booleans, JSON) for schema columns
          </label>

          {!isArrays && (
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={opts.rebuildNested}
                onChange={(e) => patch({ rebuildNested: e.target.checked })}
                className="accent-violet-600"
              />
              Rebuild nested objects from <code>a.b.c</code> column names
            </label>
          )}

          {isArrays && (
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={opts.includeHeaders}
                onChange={(e) => patch({ includeHeaders: e.target.checked })}
                className="accent-violet-600"
              />
              Write the header names as the first array
            </label>
          )}

          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={opts.backup === "single"}
              onChange={(e) => patch({ backup: e.target.checked ? "single" : "none" })}
              className="accent-violet-600"
            />
            Keep .bak of replaced files
          </label>
        </div>

        <hr className="border-zinc-100 dark:border-zinc-800" />

        <Row label="Cell text → JSON null">
          <input
            type="text"
            value={opts.nullToken ?? ""}
            onChange={(e) => patch({ nullToken: e.target.value })}
            className={`${inputCls} w-28`}
          />
        </Row>
        <Row label="Cell text → missing field">
          <input
            type="text"
            value={opts.missingToken ?? ""}
            placeholder="(empty)"
            onChange={(e) => patch({ missingToken: e.target.value })}
            className={`${inputCls} w-28`}
          />
        </Row>
        {tokensClash && (
          <p className="text-xs text-red-600 dark:text-red-400">
            The null token and missing-field text must differ.
          </p>
        )}

        {/* Nested-rebuild mapping */}
        {rebuild && mapping && (
          <div className="space-y-1">
            <p className="text-xs font-medium text-zinc-600 dark:text-zinc-300">
              Rebuilt output paths
            </p>
            <div className="max-h-40 overflow-auto rounded border border-zinc-200 dark:border-zinc-800">
              <table className="w-full border-collapse text-[11px]">
                <thead className="sticky top-0 bg-white text-left uppercase tracking-wide text-zinc-400 dark:bg-zinc-900">
                  <tr>
                    <th className="px-2 py-1 font-medium">Column</th>
                    <th className="px-2 py-1 font-medium">JSON path</th>
                  </tr>
                </thead>
                <tbody>
                  {mapping.rows.slice(0, MAX_MAPPING_ROWS).map((r, i) => (
                    <tr
                      key={`${r.header}-${i}`}
                      className={`border-t border-zinc-100 dark:border-zinc-800 ${r.conflict ? "bg-red-50 dark:bg-red-950/40" : ""}`}
                    >
                      <td className="max-w-56 truncate px-2 py-1 font-mono" title={r.header}>
                        {r.header}
                      </td>
                      <td className="px-2 py-1 font-mono text-zinc-500">
                        {r.segments.join(" › ")}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            {mapping.rows.length > MAX_MAPPING_ROWS && (
              <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
                + {(mapping.rows.length - MAX_MAPPING_ROWS).toLocaleString()} more column
                {mapping.rows.length - MAX_MAPPING_ROWS === 1 ? "" : "s"} not shown (every column is
                still checked for duplicate output paths).
              </p>
            )}
          </div>
        )}

        {conflict && <p className="text-xs text-red-600 dark:text-red-400">{conflict.message}</p>}

        <p className="text-[11px] text-zinc-500 dark:text-zinc-400">
          Output is UTF-8 with LF separators and no BOM. Duplicate output paths are rejected before
          anything is written.
        </p>
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
const selectCls =
  "rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700";
const inputCls =
  "rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700";
