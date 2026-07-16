import { writeText } from "@tauri-apps/plugin-clipboard-manager";
import { useState } from "react";

import { CLIPBOARD_WARN_CHARS, resolveCopyTarget } from "../lib/copyTarget";
import * as api from "../lib/tauri";
import { useActiveMeta, useStore } from "../store/useStore";
import type { CopyFormat } from "../types";
import { Modal } from "./Modal";

type FormatKey =
  | "tsv"
  | "csvCurrent"
  | "csvCustom"
  | "jsonObjects"
  | "jsonArrays"
  | "jsonLines"
  | "markdown"
  | "sqlValues";

const FORMATS: { key: FormatKey; label: string }[] = [
  { key: "tsv", label: "TSV (Excel-compatible)" },
  { key: "csvCurrent", label: "CSV — current document settings" },
  { key: "csvCustom", label: "CSV — custom settings" },
  { key: "jsonObjects", label: "JSON — array of objects" },
  { key: "jsonArrays", label: "JSON — array of arrays" },
  { key: "jsonLines", label: "JSON Lines" },
  { key: "markdown", label: "Markdown table" },
  { key: "sqlValues", label: "SQL VALUES rows" },
];

/**
 * Copy As (F14): serialize the selection (or every visible row) into a
 * structured clipboard format. Serialization happens in Rust; very large
 * payloads ask for confirmation before touching the system clipboard.
 */
export function CopyAsDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const selectionRect = useStore((s) => s.selectionRect);
  const selectedRows = useStore((s) => s.selectedRows);
  const selectedCols = useStore((s) => s.selectedCols);

  const hasSelection = selectionRect !== null || selectedRows.length > 0 || selectedCols.length > 0;
  const [scope, setScope] = useState<"selection" | "visible">(
    hasSelection ? "selection" : "visible",
  );
  const [format, setFormat] = useState<FormatKey>("tsv");
  const [includeHeaders, setIncludeHeaders] = useState(true);
  const [delimiter, setDelimiter] = useState(",");
  const [quoteStyle, setQuoteStyle] = useState("necessary");
  const [lineEnding, setLineEnding] = useState("lf");
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState<string | null>(null); // large payload awaiting confirm
  const [done, setDone] = useState<number | null>(null);

  if (!meta) return null;

  const buildFormat = (): CopyFormat =>
    format === "csvCustom"
      ? { type: "csvCustom", delimiter, quoteStyle, lineEnding }
      : ({ type: format } as CopyFormat);

  const finish = async (text: string) => {
    await writeText(text);
    setDone(text.length);
    setPending(null);
    setTimeout(onClose, 900);
  };

  const copy = async () => {
    const target = resolveCopyTarget(
      scope,
      selectionRect,
      selectedRows,
      selectedCols,
      meta.colCount,
    );
    if (!target) {
      setError("Nothing is selected — choose “All visible rows” or select cells first.");
      return;
    }
    setWorking(true);
    setError(null);
    try {
      const text = await api.copyAs(
        meta.id,
        target.rows,
        target.cols,
        includeHeaders,
        buildFormat(),
      );
      if (text.length > CLIPBOARD_WARN_CHARS) {
        setPending(text); // ask before flooding the system clipboard
      } else {
        await finish(text);
      }
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  };

  return (
    <Modal
      title="Copy As"
      onClose={onClose}
      footer={
        <>
          <button onClick={onClose} className={btnGhost}>
            Cancel
          </button>
          <button
            onClick={() => void copy()}
            disabled={working || pending !== null}
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            {working ? "Serializing…" : done !== null ? "Copied ✓" : "Copy"}
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <div className="flex gap-4 text-xs">
          <label className="flex items-center gap-1.5">
            <input
              type="radio"
              checked={scope === "selection"}
              disabled={!hasSelection}
              onChange={() => setScope("selection")}
              className="accent-violet-600"
            />
            Selection
          </label>
          <label className="flex items-center gap-1.5">
            <input
              type="radio"
              checked={scope === "visible"}
              onChange={() => setScope("visible")}
              className="accent-violet-600"
            />
            All visible rows ({meta.rowCount.toLocaleString()})
          </label>
          <label className="ml-auto flex items-center gap-1.5">
            <input
              type="checkbox"
              checked={includeHeaders}
              onChange={(e) => setIncludeHeaders(e.target.checked)}
              className="accent-violet-600"
            />
            Include headers
          </label>
        </div>

        <div className="grid grid-cols-2 gap-1">
          {FORMATS.map((f) => (
            <label
              key={f.key}
              className={`flex cursor-pointer items-center gap-1.5 rounded border px-2 py-1.5 text-xs ${
                format === f.key
                  ? "border-violet-500 bg-violet-50 dark:bg-violet-500/10"
                  : "border-zinc-200 dark:border-zinc-700"
              }`}
            >
              <input
                type="radio"
                checked={format === f.key}
                onChange={() => setFormat(f.key)}
                className="accent-violet-600"
              />
              {f.label}
            </label>
          ))}
        </div>

        {format === "csvCustom" && (
          <div className="flex gap-3 text-xs">
            <label className="flex items-center gap-1.5">
              Delimiter
              <input
                value={delimiter}
                maxLength={2}
                onChange={(e) => setDelimiter(e.target.value)}
                className="w-10 rounded border border-zinc-300 bg-transparent px-1 py-0.5 text-center dark:border-zinc-600"
              />
            </label>
            <label className="flex items-center gap-1.5">
              Quoting
              <select
                value={quoteStyle}
                onChange={(e) => setQuoteStyle(e.target.value)}
                className="rounded border border-zinc-300 bg-transparent px-1 py-0.5 dark:border-zinc-600"
              >
                <option value="necessary">When needed</option>
                <option value="always">Always</option>
                <option value="never">Never</option>
              </select>
            </label>
            <label className="flex items-center gap-1.5">
              Line endings
              <select
                value={lineEnding}
                onChange={(e) => setLineEnding(e.target.value)}
                className="rounded border border-zinc-300 bg-transparent px-1 py-0.5 dark:border-zinc-600"
              >
                <option value="lf">LF</option>
                <option value="crlf">CRLF</option>
              </select>
            </label>
          </div>
        )}

        {pending !== null && (
          <div className="rounded border border-amber-300 bg-amber-50 px-3 py-2 text-xs text-amber-800 dark:border-amber-500/40 dark:bg-amber-500/10 dark:text-amber-300">
            <p>
              This copy is {(pending.length / 1_000_000).toFixed(1)} million characters — placing it
              on the clipboard may be slow. Continue?
            </p>
            <div className="mt-1.5 flex justify-end gap-2">
              <button
                onClick={() => setPending(null)}
                className="rounded px-2 py-0.5 hover:bg-amber-100 dark:hover:bg-amber-500/20"
              >
                Cancel
              </button>
              <button
                onClick={() => void finish(pending)}
                className="rounded bg-amber-600 px-2 py-0.5 text-white hover:bg-amber-500"
              >
                Copy anyway
              </button>
            </div>
          </div>
        )}

        {error && <p className="text-xs text-red-600 dark:text-red-400">{error}</p>}
        {done !== null && (
          <p className="text-xs text-emerald-600 dark:text-emerald-400">
            Copied {done.toLocaleString()} characters to the clipboard.
          </p>
        )}
      </div>
    </Modal>
  );
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
