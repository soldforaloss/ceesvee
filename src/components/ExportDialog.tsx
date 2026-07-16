import { useEffect, useMemo, useState } from "react";

import { buildSplit, scopeChoices, scopeKey } from "../lib/export";
import { DELIMITER_OPTIONS, ENCODING_OPTIONS } from "../lib/labels";
import * as api from "../lib/tauri";
import { useActiveMeta, useStore } from "../store/useStore";
import type { ExportOptions, ScopeCounts, SplitOptions } from "../types";
import { Modal } from "./Modal";

/**
 * Scoped/split export (F04). Exports never touch the document's save point —
 * Ctrl+S / Save As always write the complete document through the save path.
 */
export function ExportDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const exportScoped = useStore((s) => s.exportScoped);
  const filtered = useStore((s) => s.tabs.find((t) => t.id === s.activeId)?.filtered ?? false);
  const selectionRect = useStore((s) => s.selectionRect);
  const selectedRows = useStore((s) => s.selectedRows);
  const selectedCols = useStore((s) => s.selectedCols);
  const lastExportOptions = useStore((s) => s.lastExportOptions);

  const [opts, setOpts] = useState<ExportOptions>(
    () =>
      // Per-document memory of the last export settings (F08).
      lastExportOptions ?? {
        delimiter: meta?.delimiter ?? ",",
        encoding: ENCODING_OPTIONS.some((o) => o.value === meta?.encoding)
          ? (meta?.encoding ?? "UTF-8")
          : "UTF-8",
        quoteStyle: "minimal",
        lineEnding: meta?.lineEnding ?? "lf",
        bom: meta?.hadBom ?? false,
        includeHeaders: meta?.hasHeaderRow ?? true,
        backup: "none",
      },
  );

  const choices = useMemo(
    () => scopeChoices(filtered, selectionRect, selectedRows, selectedCols),
    [filtered, selectionRect, selectedRows, selectedCols],
  );
  const [scopeIdx, setScopeIdx] = useState(() => {
    // A flow that prepared a specific scope (F28's "Export non-PII
    // columns") preselects it — the all-columns default would defeat it.
    const preferred = useStore.getState().exportPreferredScope;
    if (preferred) {
      useStore.getState().setExportPreferredScope(null);
      const at = choices.findIndex((c) => c.scope.type === preferred);
      if (at >= 0) return at;
    }
    return 0;
  });
  const scope = (choices[scopeIdx] ?? choices[0]).scope;

  const [splitKind, setSplitKind] = useState<SplitOptions["type"]>("none");
  const [rowsPerFile, setRowsPerFile] = useState(100_000);
  const [maxMegabytes, setMaxMegabytes] = useState(50);
  const [groupColumn, setGroupColumn] = useState(0);
  const [writeManifest, setWriteManifest] = useState(false);

  const [counts, setCounts] = useState<ScopeCounts | null>(null);

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
  const patch = (p: Partial<ExportOptions>) => setOpts((o) => ({ ...o, ...p }));

  const split = buildSplit(splitKind, rowsPerFile, maxMegabytes, groupColumn);
  const splitError = "error" in split ? split.error : null;

  const doExport = () => {
    if ("error" in split) return;
    void exportScoped(opts, scope, split, writeManifest);
    onClose();
  };

  return (
    <Modal
      title="Export"
      onClose={onClose}
      size="lg"
      footer={
        <>
          <button
            onClick={onClose}
            className="rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800"
          >
            Cancel
          </button>
          <button
            onClick={doExport}
            disabled={splitError !== null}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            Choose file & export
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
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

        <Row label="Split output">
          <select
            value={splitKind}
            onChange={(e) => setSplitKind(e.target.value as SplitOptions["type"])}
            className={selectCls}
          >
            <option value="none" className="dark:bg-zinc-800">
              Single file
            </option>
            <option value="maxRows" className="dark:bg-zinc-800">
              Max rows per file
            </option>
            <option value="approximateBytes" className="dark:bg-zinc-800">
              Approximate size per file
            </option>
            <option value="groupByColumn" className="dark:bg-zinc-800">
              One file per value of…
            </option>
          </select>
        </Row>

        {splitKind === "maxRows" && (
          <Row label="Rows per file">
            <input
              type="number"
              min={1}
              value={rowsPerFile}
              onChange={(e) => setRowsPerFile(Number(e.target.value))}
              className={inputCls}
            />
          </Row>
        )}
        {splitKind === "approximateBytes" && (
          <Row label="MB per file">
            <input
              type="number"
              min={1}
              value={maxMegabytes}
              onChange={(e) => setMaxMegabytes(Number(e.target.value))}
              className={inputCls}
            />
          </Row>
        )}
        {splitKind === "groupByColumn" && (
          <Row label="Group column">
            <select
              value={groupColumn}
              onChange={(e) => setGroupColumn(Number(e.target.value))}
              className={selectCls}
            >
              {meta.headers.map((h, i) => (
                <option key={i} value={i} className="dark:bg-zinc-800">
                  {h.trim() || `Column ${i + 1}`}
                </option>
              ))}
            </select>
          </Row>
        )}
        {splitError && <p className="text-xs text-red-600 dark:text-red-400">{splitError}</p>}

        <hr className="border-zinc-100 dark:border-zinc-800" />

        <Row label="Delimiter">
          <select
            value={
              DELIMITER_OPTIONS.some((o) => o.value === opts.delimiter) ? opts.delimiter : "other"
            }
            onChange={(e) =>
              patch({ delimiter: e.target.value === "other" ? opts.delimiter : e.target.value })
            }
            className={selectCls}
          >
            {DELIMITER_OPTIONS.map((o) => (
              <option key={o.value} value={o.value} className="dark:bg-zinc-800">
                {o.label}
              </option>
            ))}
            {!DELIMITER_OPTIONS.some((o) => o.value === opts.delimiter) && (
              <option value="other" className="dark:bg-zinc-800">
                Custom ({opts.delimiter})
              </option>
            )}
          </select>
        </Row>

        <Row label="Encoding">
          <select
            value={opts.encoding}
            onChange={(e) => patch({ encoding: e.target.value })}
            className={selectCls}
          >
            {ENCODING_OPTIONS.map((o) => (
              <option key={o.value} value={o.value} className="dark:bg-zinc-800">
                {o.label}
              </option>
            ))}
          </select>
        </Row>

        <Row label="Quoting">
          <Segmented
            value={opts.quoteStyle}
            options={[
              { value: "minimal", label: "Minimal" },
              { value: "always", label: "Always quote" },
            ]}
            onChange={(v) => patch({ quoteStyle: v as ExportOptions["quoteStyle"] })}
          />
        </Row>

        <Row label="Line endings">
          <Segmented
            value={opts.lineEnding}
            options={[
              { value: "lf", label: "LF (\\n)" },
              { value: "crlf", label: "CRLF (\\r\\n)" },
            ]}
            onChange={(v) => patch({ lineEnding: v as ExportOptions["lineEnding"] })}
          />
        </Row>

        <div className="flex flex-wrap gap-x-5 gap-y-2">
          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={opts.bom}
              onChange={(e) => patch({ bom: e.target.checked })}
              className="accent-violet-600"
            />
            Write BOM
          </label>

          {meta.hasHeaderRow && (
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={opts.includeHeaders}
                onChange={(e) => patch({ includeHeaders: e.target.checked })}
                className="accent-violet-600"
              />
              Include header row
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

          <label className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={writeManifest}
              onChange={(e) => setWriteManifest(e.target.checked)}
              className="accent-violet-600"
            />
            Write JSON manifest (row counts + SHA-256)
          </label>
        </div>
      </div>
    </Modal>
  );
}

const selectCls =
  "rounded border border-zinc-300 bg-transparent px-2 py-1 text-sm outline-none focus:border-violet-500 dark:border-zinc-700";
const inputCls =
  "w-32 rounded border border-zinc-300 bg-transparent px-2 py-1 text-right text-sm tabular-nums outline-none focus:border-violet-500 dark:border-zinc-700";

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="flex items-center justify-between gap-3">
      <span className="text-zinc-500 dark:text-zinc-400">{label}</span>
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
