import { useEffect, useMemo, useRef, useState } from "react";

import { containsNul, countLines, escapeCellText, utf8ByteLength } from "../lib/cellText";
import * as api from "../lib/tauri";
import { useActiveMeta, useStore } from "../store/useStore";

type Mode = "rendered" | "escaped";

/**
 * The multiline/raw cell editor (F13). Always operates on the COMPLETE cell
 * content fetched from Rust — never the grid's display text. Rendered mode
 * edits; Escaped mode is a read-only view that makes newlines, tabs,
 * non-breaking spaces, zero-width and control characters, and U+FFFD
 * visible, and can be copied without touching the stored value. Applying is
 * a single undo step; indexed documents allow inspection and copy only.
 */
export function CellEditorDialog() {
  const target = useStore((s) => s.cellEditor);
  const close = useStore((s) => s.closeCellEditor);
  const setCell = useStore((s) => s.setCell);
  const meta = useActiveMeta();

  const [value, setValue] = useState<string | null>(null);
  const [original, setOriginal] = useState<string | null>(null);
  const [mode, setMode] = useState<Mode>("rendered");
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [copied, setCopied] = useState(false);
  // Counts are debounced: recomputing byte counts per keystroke on a
  // megabyte-sized cell would stall typing.
  const [counts, setCounts] = useState({ lines: 1, chars: 0, bytes: 0 });
  // Whether the current value parses as JSON (F26: pretty-print action).
  const [isJson, setIsJson] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const readOnly = meta?.backing === "indexedReadOnly";

  // Fetch the full value whenever a target cell is set.
  useEffect(() => {
    if (!target || !meta) return;
    setValue(null);
    setOriginal(null);
    setError(null);
    setMode("rendered");
    let cancelled = false;
    api
      .getCell(meta.id, target.row, target.col)
      .then((content) => {
        if (cancelled) return;
        setValue(content);
        setOriginal(content);
        setTimeout(() => textareaRef.current?.focus(), 0);
      })
      .catch((e) => !cancelled && setError(String(e)));
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [target?.row, target?.col, meta?.id]);

  useEffect(() => {
    if (value === null) return;
    const handle = setTimeout(() => {
      setCounts({ lines: countLines(value), chars: value.length, bytes: utf8ByteLength(value) });
      const lead = value.trimStart();
      let json = false;
      if (lead.startsWith("{") || lead.startsWith("[")) {
        try {
          JSON.parse(value);
          json = true;
        } catch {
          json = false;
        }
      }
      setIsJson(json);
    }, 120);
    return () => clearTimeout(handle);
  }, [value]);

  // Escaped text is computed only when that mode is shown (large cells).
  const escaped = useMemo(
    () => (mode === "escaped" && value !== null ? escapeCellText(value) : ""),
    [mode, value],
  );

  if (!target || !meta) return null;

  const hasNul = value !== null && containsNul(value);
  const dirty = value !== null && original !== null && value !== original;

  const save = async () => {
    if (value === null || readOnly) return;
    if (hasNul) {
      setError("The value contains a NUL character, which cannot be stored — remove it first.");
      return;
    }
    setSaving(true);
    try {
      // One undo step: the ordinary set_cell path.
      await setCell(target.row, target.col, value);
      // The windowed grid cache still holds the old value (set_cell only
      // refreshes metadata); invalidate it so the saved cell repaints.
      useStore.getState().invalidateGrid();
      close();
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const copyEscaped = async () => {
    try {
      await navigator.clipboard.writeText(mode === "escaped" ? escaped : (value ?? ""));
      setCopied(true);
      setTimeout(() => setCopied(false), 1200);
    } catch (e) {
      setError(String(e));
    }
  };

  const columnName = meta.headers[target.col] || `Column ${target.col + 1}`;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/30"
      onMouseDown={close}
      role="dialog"
      aria-modal="true"
      aria-label="Cell editor"
    >
      <div
        className="flex max-h-[86vh] min-h-[320px] w-[640px] max-w-[94vw] resize flex-col overflow-auto rounded-xl border border-zinc-200 bg-white p-3 shadow-2xl dark:border-zinc-700 dark:bg-zinc-900"
        onMouseDown={(e) => e.stopPropagation()}
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            close();
          }
        }}
      >
        <div className="mb-2 flex items-center gap-2 text-xs text-zinc-500 dark:text-zinc-400">
          <span className="font-medium text-zinc-700 dark:text-zinc-200">
            Row {target.row + 1} · {columnName}
          </span>
          {readOnly && (
            <span className="rounded-full bg-amber-100 px-1.5 py-0.5 text-amber-800 dark:bg-amber-500/15 dark:text-amber-300">
              Read-only (indexed)
            </span>
          )}
          <span className="flex-1" />
          <div className="flex overflow-hidden rounded border border-zinc-200 dark:border-zinc-700">
            {(["rendered", "escaped"] as const).map((m) => (
              <button
                key={m}
                onClick={() => setMode(m)}
                className={`px-2 py-0.5 capitalize ${
                  mode === m
                    ? "bg-violet-600 text-white"
                    : "text-zinc-500 hover:bg-zinc-100 dark:hover:bg-zinc-800"
                }`}
              >
                {m}
              </button>
            ))}
          </div>
        </div>

        {value === null && !error && (
          <p className="flex-1 py-8 text-center text-sm text-zinc-400">Loading cell…</p>
        )}

        {value !== null && mode === "rendered" && (
          <textarea
            ref={textareaRef}
            value={value}
            readOnly={readOnly}
            onChange={(e) => setValue(e.target.value)}
            spellCheck={false}
            className="min-h-40 flex-1 resize-none rounded border border-zinc-200 bg-white p-2 font-mono text-sm outline-none focus:border-violet-500 dark:border-zinc-700 dark:bg-zinc-950"
          />
        )}
        {value !== null && mode === "escaped" && (
          <pre className="min-h-40 flex-1 overflow-auto whitespace-pre-wrap break-all rounded border border-zinc-200 bg-zinc-50 p-2 font-mono text-sm dark:border-zinc-700 dark:bg-zinc-950">
            {escaped}
          </pre>
        )}

        <div className="mt-2 flex items-center gap-3 text-xs text-zinc-500 dark:text-zinc-400">
          <span>{counts.lines.toLocaleString()} lines</span>
          <span>{counts.chars.toLocaleString()} chars</span>
          <span>{counts.bytes.toLocaleString()} UTF-8 bytes</span>
          {hasNul && (
            <span className="text-red-600 dark:text-red-400">contains NUL — cannot save</span>
          )}
          <span className="flex-1" />
          {isJson && mode === "rendered" && !readOnly && (
            <button
              onClick={() => {
                // Reformat in the editor only — the user reviews the result
                // and commits it with Save (one undo step), or cancels.
                try {
                  setValue(JSON.stringify(JSON.parse(value ?? ""), null, 2));
                } catch (e) {
                  setError(String(e));
                }
              }}
              className="rounded border border-zinc-200 px-2 py-1 hover:border-violet-400 dark:border-zinc-700"
            >
              Pretty-print JSON
            </button>
          )}
          <button
            onClick={() => void copyEscaped()}
            className="rounded border border-zinc-200 px-2 py-1 hover:border-violet-400 dark:border-zinc-700"
          >
            {copied ? "Copied" : mode === "escaped" ? "Copy escaped" : "Copy"}
          </button>
          <button
            onClick={close}
            className="rounded px-2 py-1 text-zinc-600 hover:bg-zinc-100 dark:text-zinc-300 dark:hover:bg-zinc-800"
          >
            Cancel
          </button>
          {!readOnly && (
            <button
              onClick={() => void save()}
              disabled={saving || value === null || !dirty || hasNul}
              className="rounded bg-violet-600 px-3 py-1 text-white hover:bg-violet-500 disabled:opacity-40"
            >
              {saving ? "Saving…" : "Save"}
            </button>
          )}
        </div>

        {error && <p className="mt-2 text-xs text-red-600 dark:text-red-400">{error}</p>}
      </div>
    </div>
  );
}
