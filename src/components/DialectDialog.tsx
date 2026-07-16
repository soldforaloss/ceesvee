import { useState } from "react";

import { parseNullTokens } from "../lib/repair";
import * as api from "../lib/tauri";
import { useActiveMeta, useStore } from "../store/useStore";
import type { CsvDialectOptions, DialectPreview } from "../types";
import { Modal } from "./Modal";

/**
 * Advanced dialect import (F18): preambles, comment lines, custom quoting
 * and escaping, multi-row headers, trailing footers, and null tokens. The
 * preview shows ORIGINAL record numbers; clicking a row sets the header
 * row. Applying reinterprets the file through the guarded reparse path —
 * a dirty document requires explicit confirmation, and saving afterwards
 * writes only the current grid (preambles are never re-added).
 */
export function DialectDialog({ onClose }: { onClose: () => void }) {
  const meta = useActiveMeta();
  const refresh = useStore((s) => s.refreshActiveDoc);

  const [delimiter, setDelimiter] = useState(",");
  const [quote, setQuote] = useState('"');
  const [quoting, setQuoting] = useState(true);
  const [doubleQuote, setDoubleQuote] = useState(true);
  const [escapeChar, setEscapeChar] = useState("");
  const [comment, setComment] = useState("");
  const [skipLeading, setSkipLeading] = useState(0);
  const [skipTrailing, setSkipTrailing] = useState(0);
  const [headerIndex, setHeaderIndex] = useState<number | null>(0);
  const [headerCount, setHeaderCount] = useState(1);
  const [joiner, setJoiner] = useState(" ");
  const [nullTokens, setNullTokens] = useState("");
  const [preview, setPreview] = useState<DialectPreview | null>(null);
  const [confirmDirty, setConfirmDirty] = useState(false);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (!meta) return null;
  const readOnly = meta.backing === "indexedReadOnly";

  const buildDialect = (): CsvDialectOptions => ({
    delimiter,
    quoteCharacter: quoting ? quote : null,
    doubleQuote,
    escapeCharacter: escapeChar === "" ? null : escapeChar,
    commentPrefix: comment === "" ? null : comment,
    skipLeadingRecords: skipLeading,
    skipTrailingRecords: skipTrailing,
    headerRowIndex: headerIndex,
    headerRowCount: headerCount,
    headerJoiner: joiner,
    nullTokens: parseNullTokens(nullTokens),
    encoding: null,
  });

  const invalidate = () => {
    setPreview(null);
    setConfirmDirty(false);
  };

  const runPreview = async () => {
    setError(null);
    try {
      setPreview(await api.previewDialect(meta.id, buildDialect()));
    } catch (e) {
      setError(String(e));
      setPreview(null);
    }
  };

  const apply = async () => {
    setWorking(true);
    setError(null);
    try {
      await api.applyDialect(meta.id, buildDialect(), meta.revision);
      await refresh();
      onClose();
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  };

  const num = (label: string, value: number, set: (n: number) => void) => (
    <label className="flex items-center gap-1.5">
      {label}
      <input
        type="number"
        min={0}
        value={value}
        onChange={(e) => {
          set(Math.max(0, Number(e.target.value)));
          invalidate();
        }}
        className="w-16 rounded border border-zinc-300 bg-transparent px-1 py-0.5 dark:border-zinc-600"
      />
    </label>
  );

  const chr = (label: string, value: string, set: (v: string) => void, placeholder = "") => (
    <label className="flex items-center gap-1.5">
      {label}
      <input
        value={value}
        maxLength={1}
        placeholder={placeholder}
        onChange={(e) => {
          set(e.target.value);
          invalidate();
        }}
        className="w-10 rounded border border-zinc-300 bg-transparent px-1 py-0.5 text-center font-mono dark:border-zinc-600"
      />
    </label>
  );

  return (
    <Modal
      title="Advanced import"
      onClose={onClose}
      size="xl"
      footer={
        <>
          {meta.dirty && (
            <label className="mr-auto flex items-center gap-1.5 text-xs text-amber-700 dark:text-amber-300">
              <input
                type="checkbox"
                checked={confirmDirty}
                onChange={(e) => setConfirmDirty(e.target.checked)}
                className="accent-amber-600"
              />
              Discard my unsaved changes and reinterpret the file
            </label>
          )}
          <button onClick={onClose} className={btnGhost}>
            Cancel
          </button>
          <button
            onClick={() => void runPreview()}
            disabled={working || readOnly}
            title={readOnly ? "Indexed documents reload through re-indexing" : undefined}
            className={btnGhost}
          >
            Preview
          </button>
          <button
            onClick={() => void apply()}
            disabled={working || readOnly || !preview || (meta.dirty && !confirmDirty)}
            title={
              !preview
                ? "Preview first"
                : meta.dirty && !confirmDirty
                  ? "Confirm discarding unsaved changes first"
                  : undefined
            }
            className="rounded bg-violet-600 px-3 py-1.5 text-sm text-white hover:bg-violet-500 disabled:opacity-40"
          >
            {working ? "Applying…" : "Reinterpret file"}
          </button>
        </>
      }
    >
      <div className="space-y-3 text-sm">
        <div className="flex flex-wrap items-center gap-3 text-xs">
          {chr("Delimiter", delimiter, setDelimiter)}
          <label className="flex items-center gap-1.5">
            <input
              type="checkbox"
              checked={quoting}
              onChange={(e) => {
                setQuoting(e.target.checked);
                invalidate();
              }}
              className="accent-violet-600"
            />
            Quoting
          </label>
          {quoting && chr("Quote", quote, setQuote)}
          {quoting && (
            <label className="flex items-center gap-1.5">
              <input
                type="checkbox"
                checked={doubleQuote}
                onChange={(e) => {
                  setDoubleQuote(e.target.checked);
                  invalidate();
                }}
                className="accent-violet-600"
              />
              "" escapes a quote
            </label>
          )}
          {chr("Escape", escapeChar, setEscapeChar, "–")}
          {chr("Comment", comment, setComment, "–")}
        </div>
        <div className="flex flex-wrap items-center gap-3 text-xs">
          {num("Skip first", skipLeading, setSkipLeading)}
          {num("Skip last", skipTrailing, setSkipTrailing)}
          <label className="flex items-center gap-1.5">
            <input
              type="checkbox"
              checked={headerIndex !== null}
              onChange={(e) => {
                setHeaderIndex(e.target.checked ? 0 : null);
                invalidate();
              }}
              className="accent-violet-600"
            />
            Header row
          </label>
          {headerIndex !== null && (
            <>
              {num("at record", headerIndex, (n) => setHeaderIndex(n))}
              {num("spanning", headerCount, (n) => setHeaderCount(Math.max(1, n)))}
              {headerCount > 1 && (
                <label className="flex items-center gap-1.5">
                  joined by
                  <input
                    value={joiner}
                    onChange={(e) => {
                      setJoiner(e.target.value);
                      invalidate();
                    }}
                    className="w-14 rounded border border-zinc-300 bg-transparent px-1 py-0.5 font-mono dark:border-zinc-600"
                  />
                </label>
              )}
            </>
          )}
          <label className="flex items-center gap-1.5">
            Null tokens
            <input
              value={nullTokens}
              onChange={(e) => {
                setNullTokens(e.target.value);
                invalidate();
              }}
              placeholder="NA, N/A, null"
              className="w-40 rounded border border-zinc-300 bg-transparent px-1.5 py-0.5 dark:border-zinc-600"
            />
          </label>
        </div>

        {error && <p className="text-xs text-red-600 dark:text-red-400">{error}</p>}

        {preview && (
          <div className="space-y-1.5 rounded bg-zinc-50 p-2 text-xs dark:bg-zinc-900">
            <p className="font-medium">
              {preview.totalRows.toLocaleString()} rows × {preview.nCols} columns ·{" "}
              {preview.encoding}
              {preview.nullTokenCells > 0 &&
                ` · ${preview.nullTokenCells.toLocaleString()} null-token cells (raw text kept)`}
            </p>
            {preview.duplicateHeaders.length > 0 && (
              <p className="text-amber-600 dark:text-amber-400">
                Duplicate combined headers (made unique on apply):{" "}
                {preview.duplicateHeaders.join(", ")}
              </p>
            )}
            <div className="max-h-[36vh] overflow-auto">
              <table className="w-full text-left font-mono text-[11px]">
                <thead className="sticky top-0 bg-zinc-50 text-zinc-400 dark:bg-zinc-900">
                  <tr>
                    <th className="pr-2 font-normal">rec#</th>
                    {(preview.headers ?? Array.from({ length: preview.nCols })).map((h, i) => (
                      <th key={i} className="max-w-[10rem] truncate pr-2 font-normal">
                        {(h as string) || `Column ${i + 1}`}
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {preview.sample.map((row, i) => (
                    <tr key={i} className="text-zinc-600 dark:text-zinc-300">
                      <td className="pr-2 text-zinc-400">{preview.originalNumbers[i]}</td>
                      {row.map((cell, j) => (
                        <td key={j} className="max-w-[10rem] truncate pr-2">
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
        {!preview && (
          <p className="py-4 text-center text-xs text-zinc-400">
            Configure the dialect and preview — original record numbers make skipped preambles
            visible.
          </p>
        )}
      </div>
    </Modal>
  );
}

const btnGhost =
  "rounded px-3 py-1.5 text-sm text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800";
